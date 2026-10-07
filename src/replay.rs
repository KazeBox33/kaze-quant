use crate::engine::{Engine, EventSink};
use crate::strategy::{Strategy, StrategyView};
use crate::types::*;
use std::fmt;
use std::io::{self, BufRead, Read, Write};

pub const CSV_HEADER: &str = "sequence,timestamp_ns,bid,ask,bid_quantity,ask_quantity";
const MAX_LINE: u64 = 4096;

#[derive(Debug)]
pub struct ReplayError {
    pub line: usize,
    pub message: String,
}
impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CSV line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for ReplayError {}

/// 一次只持有一行，并复用 String；有明确的单行大小上限。
pub struct QuoteReader<R> {
    reader: R,
    buffer: String,
    line: usize,
    done: bool,
}
impl<R: BufRead> QuoteReader<R> {
    pub fn new(reader: R) -> Result<Self, ReplayError> {
        let mut this = Self {
            reader,
            buffer: String::with_capacity(128),
            line: 0,
            done: false,
        };
        this.read_line()?;
        if this.buffer.trim_end_matches(['\r', '\n']) != CSV_HEADER {
            return Err(this
                .error("header must be sequence,timestamp_ns,bid,ask,bid_quantity,ask_quantity"));
        }
        Ok(this)
    }
    fn error(&self, message: impl Into<String>) -> ReplayError {
        ReplayError {
            line: self.line,
            message: message.into(),
        }
    }
    fn read_line(&mut self) -> Result<usize, ReplayError> {
        self.buffer.clear();
        self.line += 1;
        let count = (&mut self.reader)
            .take(MAX_LINE + 1)
            .read_line(&mut self.buffer)
            .map_err(|e| self.error(e.to_string()))?;
        if count as u64 > MAX_LINE {
            return Err(self.error("line exceeds 4096 bytes"));
        }
        Ok(count)
    }
    fn parse(&self) -> Result<Quote, ReplayError> {
        let mut numbers = [0u64; 6];
        let mut fields = self.buffer.trim_end_matches(['\r', '\n']).split(',');
        for (index, number) in numbers.iter_mut().enumerate() {
            let field = fields
                .next()
                .ok_or_else(|| self.error("expected exactly 6 fields"))?;
            *number = field.parse().map_err(|_| {
                self.error(format!("field {} must be an unsigned integer", index + 1))
            })?;
        }
        if fields.next().is_some() {
            return Err(self.error("expected exactly 6 fields"));
        }
        let q = Quote {
            sequence: numbers[0],
            timestamp_ns: numbers[1],
            bid: Price::new(numbers[2]).map_err(|e| self.error(e))?,
            ask: Price::new(numbers[3]).map_err(|e| self.error(e))?,
            bid_quantity: numbers[4],
            ask_quantity: numbers[5],
        };
        q.validate().map_err(|e| self.error(e))?;
        Ok(q)
    }
}
impl<R: BufRead> Iterator for QuoteReader<R> {
    type Item = Result<Quote, ReplayError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.read_line() {
            Ok(0) => {
                self.done = true;
                None
            }
            Ok(_) => {
                let result = self.parse();
                if result.is_err() {
                    self.done = true;
                }
                Some(result)
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplaySummary {
    pub strategy_rejections: u64,
    pub failed_cancels: u64,
}

pub fn replay<R: BufRead, S: Strategy>(
    reader: QuoteReader<R>,
    engine: &mut Engine,
    strategy: &mut S,
    sink: &mut impl EventSink,
) -> Result<ReplaySummary, ReplayError> {
    let mut summary = ReplaySummary::default();
    let mut seen = false;
    for (index, item) in reader.enumerate() {
        let q = item?;
        seen = true;
        engine.on_quote(q, sink).map_err(|message| ReplayError {
            line: index + 2,
            message: message.into(),
        })?;
        let action = strategy.on_quote(StrategyView {
            quote: q,
            account: engine.account(),
            active_orders: engine.active_count(),
        });
        match action {
            Action::Submit(request) => match engine.submit(request, sink) {
                Ok(_) => (),
                Err(RejectReason::HistoryLimit) => return Err(ReplayError {
                    line: index + 2,
                    message:
                        "order history capacity reached; split the replay or increase max_orders"
                            .into(),
                }),
                Err(_) => summary.strategy_rejections += 1,
            },
            Action::Cancel(id) => {
                if !engine.cancel(id, sink) {
                    summary.failed_cancels += 1;
                }
            }
            Action::SubmitConditional(_) | Action::CancelConditional(_) => {
                return Err(ReplayError {
                    line: index + 2,
                    message: "conditional actions require PaperRuntime".into(),
                });
            }
            Action::None => (),
        }
    }
    if !seen {
        return Err(ReplayError {
            line: 2,
            message: "input contains no quotes".into(),
        });
    }
    engine.finish(sink);
    Ok(summary)
}

/// CSV 事件流与统计解耦，文件由 CLI 拥有；错误被记录并在完成前检查。
pub struct TraceWriter<W> {
    writer: W,
    error: Option<io::Error>,
}
impl<W: Write> TraceWriter<W> {
    pub fn new(mut writer: W) -> io::Result<Self> {
        writeln!(
            writer,
            "event,order_id,sequence,timestamp_ns,side,quantity,price_minor,fee_minor,reason"
        )?;
        Ok(Self {
            writer,
            error: None,
        })
    }
    pub fn finish(mut self) -> io::Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }
        self.writer.flush()
    }
}
impl<W: Write> EventSink for TraceWriter<W> {
    fn emit(&mut self, event: Event) {
        if self.error.is_some() {
            return;
        }
        let result = match event {
            Event::Accepted { order_id, sequence } => {
                writeln!(self.writer, "accepted,{},{sequence},,,,,,", order_id.0)
            }
            Event::Rejected { order_id, reason } => {
                writeln!(self.writer, "rejected,{},,,,,,,{reason:?}", order_id.0)
            }
            Event::Cancelled { order_id, sequence } => {
                writeln!(self.writer, "cancelled,{},{sequence},,,,,,", order_id.0)
            }
            Event::Fill(f) => writeln!(
                self.writer,
                "fill,{},{},{},{:?},{},{},{},",
                f.order_id.0,
                f.sequence,
                f.timestamp_ns,
                f.side,
                f.quantity,
                f.price.units(),
                f.fee
            ),
        };
        if let Err(e) = result {
            self.error = Some(e);
        }
    }
}
