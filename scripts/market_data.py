#!/usr/bin/env python3
"""官方 bookTicker → 整数 lot 报价；仅 Python 标准库，无密钥。"""
import argparse
import csv
import hashlib
import io
import json
from decimal import Decimal
from pathlib import Path
import urllib.request
import zipfile

VERSION = "binance-bookticker-prefix-v1"
BASE = "https://data.binance.vision/data/futures/um/daily/bookTicker/BTCUSDT/"
NAME = "BTCUSDT-bookTicker-2024-01-01.zip"
PINNED_SHA = "f9d281b949ac10aa59af3a7b07f41b4e640a8ecc76210f97c4c25ae7c8173e7c"
MONEY_SCALE, QUANTITY_SCALE = 100_000_000, 100_000

def sha_file(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        while chunk := f.read(1024 * 1024): h.update(chunk)
    return h.hexdigest()

def exact_price(s):
    n = Decimal(s) * MONEY_SCALE / QUANTITY_SCALE
    if n != n.to_integral_value() or not 0 < n <= 10**12:
        raise ValueError(f"non-exact or out-of-range price: {s}")
    return int(n)

def quantity(s):
    n = Decimal(s) * QUANTITY_SCALE
    if not 0 <= n <= 10**9: raise ValueError(f"quantity out of range: {s}")
    return int(n)  # 对正数向下量化，绝不扩大可成交数量。

def prepare(directory, count):
    directory.mkdir(parents=True, exist_ok=True)
    archive = directory / NAME
    official = urllib.request.urlopen(BASE + NAME + ".CHECKSUM", timeout=45).read().decode()
    if official.split()[0] != PINNED_SHA: raise ValueError("official archive changed; explicit dataset version required")
    if not archive.exists():
        partial = archive.with_suffix(".partial")
        size = 0
        with urllib.request.urlopen(BASE + NAME, timeout=45) as response, partial.open("wb") as f:
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                if size > 256 * 1024 * 1024: raise ValueError("archive capacity exceeded")
                f.write(chunk)
        if sha_file(partial) != PINNED_SHA: raise ValueError("download checksum mismatch")
        partial.rename(archive)
    if sha_file(archive) != PINNED_SHA: raise ValueError("cached archive checksum mismatch")
    output = directory / f"btc-quotes-{count}.csv"
    partial = output.with_suffix(".partial")
    hprefix = hashlib.sha256()
    stats = dict(rows=0, clock_clamps=0, quantity_rounds=0, duplicate_update_ids=0)
    previous_clock, previous_id = 0, None
    with zipfile.ZipFile(archive) as z:
        if z.namelist() != [NAME.replace(".zip", ".csv")]: raise ValueError("unexpected archive members")
        if z.infolist()[0].file_size > 2 * 1024**3: raise ValueError("uncompressed capacity exceeded")
        with z.open(z.namelist()[0]) as source, partial.open("w", newline="") as target:
            header = source.readline(); hprefix.update(header)
            if header.decode().strip() != "update_id,best_bid_price,best_bid_qty,best_ask_price,best_ask_qty,transaction_time,event_time": raise ValueError("unexpected source header")
            writer = csv.writer(target, lineterminator="\n")
            writer.writerow(["sequence", "timestamp_ns", "bid", "ask", "bid_quantity", "ask_quantity"])
            for line in source:
                if len(line) > 4096: raise ValueError("source line capacity exceeded")
                hprefix.update(line)
                row = next(csv.reader([line.decode()]))
                if len(row) != 7: raise ValueError("source column mismatch")
                update_id, bid, bqty, ask, aqty, tx, event = row
                uid = int(update_id)
                clock = int(event) * 1_000_000  # 此固定 2024 futures 文件字段为毫秒。
                if not 0 <= int(tx) <= int(event) or clock >= 2**64: raise ValueError("invalid source timestamp")
                if clock < previous_clock: stats["clock_clamps"] += 1
                clock = max(previous_clock, clock)
                if previous_id == uid: stats["duplicate_update_ids"] += 1
                for raw in [bqty, aqty]:
                    if Decimal(raw) * QUANTITY_SCALE != (Decimal(raw) * QUANTITY_SCALE).to_integral_value(): stats["quantity_rounds"] += 1
                pb, pa = exact_price(bid), exact_price(ask)
                if pb > pa: raise ValueError("crossed quote")
                stats["rows"] += 1
                writer.writerow([stats["rows"], clock, pb, pa, quantity(bqty), quantity(aqty)])
                if stats["rows"] == 1: stats.update(first_event_ns=clock, first_update_id=uid)
                previous_clock, previous_id = clock, uid
                if stats["rows"] == count: break
    if stats["rows"] != count: raise ValueError("source shorter than requested prefix")
    partial.replace(output)
    manifest = dict(transform_version=VERSION, source_url=BASE + NAME,
        checksum_url=BASE + NAME + ".CHECKSUM", archive_sha256=PINNED_SHA,
        archive_bytes=archive.stat().st_size, selection="contiguous archive prefix; no timestamp sort or price filter",
        source_prefix_sha256=hprefix.hexdigest(), normalized_sha256=sha_file(output),
        normalized_bytes=output.stat().st_size, units=dict(currency="USDT", money_scale=MONEY_SCALE, quantity_scale=QUANTITY_SCALE),
        timestamp_rule="event_time ms to ns; clamp regressions to previous observed event_time; record every clamp",
        sequence_rule="one-based archive row index; exchange update IDs need not be contiguous",
        last_event_ns=previous_clock, last_update_id=previous_id, **stats)
    output.with_suffix(".manifest.json").write_text(json.dumps(manifest, indent=2)+"\n")
    print(json.dumps(manifest, indent=2))

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, default=Path("reports/datasets"))
    parser.add_argument("--rows", type=int, default=1_000_000)
    args = parser.parse_args()
    if not 1 <= args.rows <= 10_000_000: parser.error("rows must be 1..10000000")
    prepare(args.directory, args.rows)
