#[path = "../examples/support/pulse.rs"]
mod pulse;
use kaze_quant::config::{PaperConfig, StrategyConfig};
use kaze_quant::paper::{Command, Envelope, PaperRuntime};
use kaze_quant::registry::StrategyRegistry;
use kaze_quant::store::{SqliteSession, StoreOptions};
use kaze_quant::types::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn config() -> PaperConfig {
    let mut c: PaperConfig = serde_json::from_str(include_str!("../configs/paper.json")).unwrap();
    c.markets.truncate(1);
    c.markets[0].strategy = StrategyConfig::Registered {
        name: "pulse".into(),
        version: 1,
        parameters: serde_json::json!({"period":4,"quantity":1}),
    };
    c
}
fn command(seq: u64) -> Envelope {
    Envelope {
        seq,
        command: Command::Quote {
            market: 0,
            quote: Quote {
                sequence: seq,
                timestamp_ns: seq * 1000000,
                bid: Price::new(99).unwrap(),
                ask: Price::new(100).unwrap(),
                bid_quantity: 10,
                ask_quantity: 10,
            },
        },
    }
}
#[test]
fn custom_registered_strategy_survives_every_checkpoint() {
    let c = config();
    let r = Arc::new(pulse::registry());
    let mut reference = PaperRuntime::new_with_registry(c.clone(), r.clone()).unwrap();
    let mut resumed = PaperRuntime::new_with_registry(c.clone(), r.clone()).unwrap();
    for i in 1..=40 {
        assert_eq!(
            reference.process(&command(i)).unwrap(),
            resumed.process(&command(i)).unwrap()
        );
        resumed =
            PaperRuntime::restore_with_registry(c.clone(), resumed.snapshot().unwrap(), r.clone())
                .unwrap();
    }
    assert!(resumed.report().markets[0].metrics.fills > 0);
    assert_eq!(reference.report(), resumed.report());
}
#[test]
fn unknown_factory_version_and_malformed_state_fail_closed() {
    let c = config();
    assert!(PaperRuntime::new(c.clone()).is_err());
    let r = Arc::new(pulse::registry());
    let runtime = PaperRuntime::new_with_registry(c.clone(), r.clone()).unwrap();
    let mut value = serde_json::to_value(runtime.snapshot().unwrap()).unwrap();
    value["markets"][0]["strategy"]["registered"]["state"]["seen"] = serde_json::json!(u64::MAX);
    assert!(
        PaperRuntime::restore_with_registry(
            c.clone(),
            serde_json::from_value(value).unwrap(),
            r.clone()
        )
        .is_err()
    );
    let mut changed = c;
    changed.markets[0].strategy = StrategyConfig::Registered {
        name: "pulse".into(),
        version: 2,
        parameters: serde_json::json!({"period":4,"quantity":1}),
    };
    assert!(PaperRuntime::new_with_registry(changed, r).is_err());
}
#[test]
fn sqlite_custom_state_is_atomic_and_full_replay_verifies() {
    let path = std::env::temp_dir().join(format!(
        "kaze-registry-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    let db = path.join("run.db");
    let c = config();
    let registry = Arc::new(pulse::registry());
    for chunk in (1..=32).collect::<Vec<_>>().chunks(4) {
        let mut s = SqliteSession::open_with_registry(
            &db,
            c.clone(),
            StoreOptions::default(),
            registry.clone(),
        )
        .unwrap();
        s.execute_batch(&chunk.iter().map(|&seq| command(seq)).collect::<Vec<_>>())
            .unwrap();
        s.verify_full().unwrap();
    }
    let mut s =
        SqliteSession::open_with_registry(&db, c, StoreOptions::default(), registry).unwrap();
    let before = s.runtime().report();
    let bad = Envelope {
        seq: 35,
        command: Command::Halt {},
    };
    assert!(s.execute_batch(&[command(33), bad]).is_err());
    assert_eq!(s.runtime().report(), before);
    assert_eq!(s.verify_full().unwrap(), 32);
    drop(s);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn identity_and_parameter_size_are_bounded() {
    assert!(
        StrategyRegistry::default()
            .register("bad name", 1, |_, _| Err("unused".into()))
            .is_err()
    );
    let mut c = config();
    if let StrategyConfig::Registered { parameters, .. } = &mut c.markets[0].strategy {
        *parameters = serde_json::json!({"huge":"x".repeat(8193)});
    }
    assert!(c.validate().is_err());
}
#[test]
fn invalid_custom_post_state_never_commits_candidate() {
    use kaze_quant::paper::PaperError;
    use kaze_quant::registry::CustomCheckpoint;
    use kaze_quant::strategy::{Strategy, StrategyView};
    struct Bad(bool);
    impl Strategy for Bad {
        fn on_quote(&mut self, _: StrategyView<'_>) -> Action {
            self.0 = true;
            Action::None
        }
        fn custom_checkpoint(&self) -> Option<CustomCheckpoint> {
            Some(CustomCheckpoint {
                name: "bad".into(),
                version: 1,
                state: serde_json::json!({"invalid":self.0}),
            })
        }
    }
    fn factory(
        _: &serde_json::Value,
        state: Option<&serde_json::Value>,
    ) -> Result<Box<dyn Strategy>, PaperError> {
        if state.is_some_and(|s| s["invalid"] != false) {
            return Err("invalid post-state".into());
        }
        Ok(Box::new(Bad(false)))
    }
    let mut registry = StrategyRegistry::default();
    registry.register("bad", 1, factory).unwrap();
    let registry = Arc::new(registry);
    let mut c = config();
    c.markets[0].strategy = StrategyConfig::Registered {
        name: "bad".into(),
        version: 1,
        parameters: serde_json::json!({}),
    };
    let path = std::env::temp_dir().join(format!("kaze-bad-checkpoint-{}", std::process::id()));
    std::fs::create_dir(&path).unwrap();
    let mut s = SqliteSession::open_with_registry(
        &path.join("run.db"),
        c,
        StoreOptions::default(),
        registry,
    )
    .unwrap();
    let before = s.runtime().report();
    assert!(s.execute_batch(&[command(1)]).is_err());
    assert_eq!(s.runtime().report(), before);
    assert_eq!(s.verify_full().unwrap(), 0);
    drop(s);
    std::fs::remove_dir_all(path).unwrap();
}
