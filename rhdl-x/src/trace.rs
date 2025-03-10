use rand::random;
use rhdl::{
    core::{note_pop_path, note_push_path, types::note, DiscriminantAlignment},
    prelude::*,
};
use std::{
    any::Any,
    collections::{hash_map::Entry, BTreeMap},
    hash::{Hash, Hasher},
    io::Write,
    iter::repeat,
};

use smallvec::SmallVec;
use vcd::IdCode;

use crate::{bitvector::BitVector, Digital, Kind, NoteKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraceBit {
    Zero,
    One,
    X,
    Z,
}

type TimeSeriesHash = u32;

// This Trait object captures the interface back to the VCD writer.
trait VCDWrite {
    fn timescale(&mut self, magnitude: u32, unit: vcd::TimescaleUnit) -> std::io::Result<()>;
    fn add_module(&mut self, name: &str) -> std::io::Result<()>;
    fn upscope(&mut self) -> std::io::Result<()>;
    fn enddefinitions(&mut self) -> std::io::Result<()>;
    fn timestamp(&mut self, time: u64) -> std::io::Result<()>;
    fn add_wire(&mut self, width: u32, name: &str) -> std::io::Result<IdCode>;
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()>;
}

impl<W: Write> VCDWrite for vcd::Writer<W> {
    fn timescale(&mut self, magnitude: u32, unit: vcd::TimescaleUnit) -> std::io::Result<()> {
        self.timescale(magnitude, unit)
    }
    fn add_module(&mut self, name: &str) -> std::io::Result<()> {
        self.add_module(name)
    }
    fn upscope(&mut self) -> std::io::Result<()> {
        self.upscope()
    }
    fn enddefinitions(&mut self) -> std::io::Result<()> {
        self.enddefinitions()
    }
    fn timestamp(&mut self, time: u64) -> std::io::Result<()> {
        self.timestamp(time)
    }
    fn add_wire(&mut self, width: u32, name: &str) -> std::io::Result<IdCode> {
        self.add_wire(width, name)
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.writer().write_all(buf)
    }
}

// This trait object captures those methods that are needed to walk the time series.
trait TimeSeriesWalk {
    fn cursor(
        &self,
        details: &TimeSeriesDetails,
        name: &str,
        writer: &mut dyn VCDWrite,
    ) -> Option<Cursor>;
    fn advance_cursor(&self, cursor: &mut Cursor);
    fn write_vcd(&self, cursor: &mut Cursor, writer: &mut dyn VCDWrite) -> anyhow::Result<()>;
}

//pub type TraceValue = SmallVec<[TraceBit; 16]>;
pub type TraceValue = Vec<TraceBit>;

fn bits_to_vcd(value: &TraceValue, buf: &mut SmallVec<[u8; 64]>) {
    value.iter().rev().for_each(|b| match b {
        TraceBit::Zero => buf.push(b'0'),
        TraceBit::One => buf.push(b'1'),
        TraceBit::X => buf.push(b'x'),
        TraceBit::Z => buf.push(b'z'),
    });
}

struct Cursor {
    next_time: Option<u64>,
    hash: TimeSeriesHash,
    ptr: usize,
    code: IdCode,
    code_as_bytes: Vec<u8>,
}

trait AsAny {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: Digital> AsAny for TimeSeries<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

struct TimeSeries<T: Digital> {
    values: Vec<(u64, T)>,
    kind: Kind,
}

struct TimeSeriesDetails {
    hash: TimeSeriesHash,
    path: Vec<&'static str>,
    key: String,
}

impl<T: Digital> TimeSeries<T> {
    fn new(time: u64, value: T, kind: Kind) -> Self {
        let mut values = Vec::with_capacity(1_000_000);
        values.push((time, value));
        TimeSeries { values, kind }
    }
    fn push_if_changed(&mut self, time: u64, value: T) {
        if let Some((_, last_value)) = self.values.last() {
            if last_value == &value {
                return;
            }
        }
        self.values.push((time, value));
    }
    fn as_walker(&self) -> &dyn TimeSeriesWalk {
        self
    }
}

trait AnyTimeSeries: AsAny + TimeSeriesWalk {}

impl<T: Digital> AnyTimeSeries for TimeSeries<T> {}

impl<T: Digital> TimeSeriesWalk for TimeSeries<T> {
    fn cursor(
        &self,
        details: &TimeSeriesDetails,
        name: &str,
        writer: &mut dyn VCDWrite,
    ) -> Option<Cursor> {
        let name_sanitized = name.replace("::", "__");
        let code = writer.add_wire(T::BITS as u32, &name_sanitized).ok()?;
        self.values.first().map(|x| Cursor {
            next_time: Some(x.0),
            hash: details.hash,
            ptr: 0,
            code,
            code_as_bytes: code.to_string().into_bytes(),
        })
    }
    fn advance_cursor(&self, cursor: &mut Cursor) {
        cursor.ptr += 1;
        if let Some((time, _)) = self.values.get(cursor.ptr) {
            cursor.next_time = Some(*time);
        } else {
            cursor.next_time = None;
        }
    }
    fn write_vcd(&self, cursor: &mut Cursor, writer: &mut dyn VCDWrite) -> anyhow::Result<()> {
        let mut sbuf = SmallVec::<[u8; 64]>::new();
        if let Some((_time, value)) = self.values.get(cursor.ptr) {
            sbuf.push(b'b');
            sbuf.extend(value.bin().into_iter().map(|b| match b {
                true => b'1',
                false => b'0',
            }));
            sbuf.push(b' ');
            writer.write_all(&sbuf[..])?;
            writer.write_all(&cursor.code_as_bytes)?;
            writer.write_all(b"\n")?;
            self.advance_cursor(cursor);
            Ok(())
        } else {
            Err(anyhow::anyhow!("No more values"))
        }
    }
}

#[derive(Default)]
pub struct TraceDB {
    db: fnv::FnvHashMap<TimeSeriesHash, Box<dyn AnyTimeSeries>>,
    details: fnv::FnvHashMap<TimeSeriesHash, TimeSeriesDetails>,
    path: Vec<&'static str>,
    time: u64,
    dummy: u64,
}

impl TraceDB {
    fn push_path(&mut self, name: &'static str) {
        self.path.push(name);
    }
    fn pop_path(&mut self) {
        self.path.pop();
    }
    fn key_hash(&self, key: &impl NoteKey) -> TimeSeriesHash {
        let mut hasher = fnv::FnvHasher::default();
        let key = (&self.path[..], key);
        key.hash(&mut hasher);
        hasher.finish() as TimeSeriesHash
    }
    fn set_time(&mut self, time: u64) {
        self.time = time;
    }
    fn trace(&mut self, key: impl NoteKey, value: &impl Digital) {
        let hash = self.key_hash(&key);
        match self.db.entry(hash) {
            Entry::Occupied(mut entry) => {
                entry
                    .get_mut()
                    .as_any_mut()
                    .downcast_mut::<TimeSeries<_>>()
                    .unwrap_or_else(|| panic!("Type mismatch for {}", key.as_string()))
                    .push_if_changed(self.time, *value);
            }
            Entry::Vacant(entry) => {
                let kind = value.kind();
                eprintln!(
                    "Defining new time series: {path:?} {key} {kind:?} {bits}",
                    path = self.path,
                    key = key.as_string(),
                    kind = kind,
                    bits = kind.bits(),
                );
                let details = TimeSeriesDetails {
                    hash,
                    path: self.path.clone(),
                    key: key.as_string().to_string(),
                };
                self.details.insert(hash, details);
                entry.insert(Box::new(TimeSeries::new(self.time, *value, kind)));
            }
        }
    }

    fn setup_cursor<W: Write>(
        &self,
        name: &str,
        details: &TimeSeriesDetails,
        writer: &mut vcd::Writer<W>,
    ) -> Option<Cursor> {
        self.db
            .get(&details.hash)
            .and_then(|series| series.cursor(details, name, writer))
    }
    fn write_advance_cursor<W: Write>(
        &self,
        cursor: &mut Cursor,
        writer: &mut vcd::Writer<W>,
    ) -> anyhow::Result<()> {
        let series = self.db.get(&cursor.hash).unwrap();
        series.write_vcd(cursor, writer)
    }
    fn setup_cursors<W: Write>(
        &self,
        name: &str,
        scope: &Scope,
        cursors: &mut Vec<Cursor>,
        writer: &mut vcd::Writer<W>,
    ) -> anyhow::Result<()> {
        writer.add_module(name)?;
        for (name, hash) in &scope.signals {
            let details = self.details.get(hash).unwrap();
            if let Some(cursor) = self.setup_cursor(name, details, writer) {
                cursors.push(cursor);
            }
        }
        for (name, child) in &scope.children {
            self.setup_cursors(name, child, cursors, writer)?;
        }
        writer.upscope()?;
        Ok(())
    }
    pub fn dump_vcd<W: Write>(&self, w: W) -> anyhow::Result<()> {
        let mut writer = vcd::Writer::new(w);
        writer.timescale(1, vcd::TimescaleUnit::PS)?;
        let root_scope = hierarchical_walk(self.details.iter().map(|(hash, details)| TSItem {
            path: &details.path,
            name: &details.key,
            hash: *hash,
        }));
        let mut cursors = vec![];
        self.setup_cursors("top", &root_scope, &mut cursors, &mut writer)?;
        writer.enddefinitions()?;
        writer.timestamp(0)?;
        let mut current_time = 0;
        let mut keep_running = true;
        while keep_running {
            keep_running = false;
            let mut next_time = !0;
            let mut found_match = true;
            while found_match {
                found_match = false;
                for cursor in &mut cursors {
                    if cursor.next_time == Some(current_time) {
                        self.write_advance_cursor(cursor, &mut writer)?;
                        found_match = true;
                    } else if let Some(time) = cursor.next_time {
                        next_time = next_time.min(time);
                    }
                    if cursor.next_time.is_some() {
                        keep_running = true;
                    }
                }
            }
            if next_time != !0 {
                current_time = next_time;
                writer.timestamp(current_time)?;
            }
        }
        Ok(())
    }
}

struct TSItem<'a> {
    path: &'a [&'static str],
    name: &'a str,
    hash: TimeSeriesHash,
}

#[derive(Default)]
struct Scope {
    children: BTreeMap<&'static str, Box<Scope>>,
    signals: BTreeMap<String, TimeSeriesHash>,
}

fn hierarchical_walk<'a>(paths: impl Iterator<Item = TSItem<'a>>) -> Scope {
    let mut root = Scope::default();
    for ts_item in paths {
        let mut folder = &mut root;
        for item in ts_item.path {
            if !folder.children.contains_key(item) {
                let new_folder = Box::new(Scope::default());
                folder.children.insert(item, new_folder);
            }
            folder = folder.children.get_mut(item).unwrap();
        }
        folder.signals.insert(ts_item.name.into(), ts_item.hash);
    }
    root
}

#[test]
fn test_trace_db() {
    let mut db = TraceDB::default();
    for i in 0..10 {
        db.set_time(i * 1000);
        db.push_path("fn1");
        db.push_path("fn2");
        db.trace("a", &true);
        db.pop_path();
        db.trace("a", &b6(i as u128));
        db.pop_path();
    }
    let mut vcd = vec![];
    db.dump_vcd(&mut vcd).unwrap();
    std::fs::write("trace.vcd", vcd).unwrap();
}

#[test]
fn test_bit_shuffling_iter_chain_time() {
    let tic = std::time::Instant::now();
    let mut total_bits = 0;
    let host_bits = (0..200).map(|_| rand::random::<bool>()).collect::<Vec<_>>();
    for i in 0..10_000_000 {
        let len = rand::random::<usize>() % 100 + 4;
        let width = rand::random::<usize>() % len;
        let mut orig = host_bits.iter().take(len).copied().collect::<Vec<_>>();
        orig.resize(110, false);
        let right = orig.iter().skip(width).take(len - width);
        let left = orig.iter().take(width);
        let bits = right.chain(left).copied().collect::<Vec<_>>();
        if let Some(bit) = bits.last() {
            if *bit {
                total_bits += 1;
            }
        }
    }
    let toc = std::time::Instant::now();
    eprintln!("BitShuffling Iter Chain: {:?} {total_bits}", toc - tic);
}

#[test]
fn test_bit_shuffling_split_time() {
    let tic = std::time::Instant::now();
    let mut total_bits = 0;
    let host_bits = (0..200).map(|_| rand::random::<bool>()).collect::<Vec<_>>();
    for i in 0..10_000_000 {
        let len = rand::random::<usize>() % 100 + 4;
        let width = rand::random::<usize>() % len;
        let mut orig = host_bits.iter().take(len).copied().collect::<Vec<_>>();
        orig.resize(110, false);
        let (left, right) = orig.split_at(width);
        let bits = right.iter().chain(left.iter()).copied().collect::<Vec<_>>();
        if let Some(bit) = bits.last() {
            if *bit {
                total_bits += 1;
            }
        }
    }
    let toc = std::time::Instant::now();
    eprintln!("BitShuffling: {:?} {total_bits}", toc - tic);
}

#[test]
fn test_bit_shuffling_move_time() {
    let tic = std::time::Instant::now();
    let mut total_bits = 0;
    let host_bits = (0..200).map(|_| rand::random::<bool>()).collect::<Vec<_>>();
    for i in 0..10_000_000 {
        let len = rand::random::<usize>() % 100 + 4;
        let width = rand::random::<usize>() % len;
        let mut orig = host_bits.iter().take(len).copied().collect::<Vec<_>>();
        orig.resize(110, false);
        let (left, right) = orig.split_at(width);
        let bits = [right, left].concat();
        if let Some(bit) = bits.last() {
            if *bit {
                total_bits += 1;
            }
        }
    }
    let toc = std::time::Instant::now();
    eprintln!("BitShuffling (MOVE): {:?} {total_bits}", toc - tic);
}

#[test]
fn test_bit_shuffling_min_time() {
    let tic = std::time::Instant::now();
    let mut total_bits = 0;
    let host_bits = (0..200).map(|_| rand::random::<bool>()).collect::<Vec<_>>();
    for i in 0..10_000_000 {
        let len = rand::random::<usize>() % 100 + 4;
        let width = rand::random::<usize>() % len;
        let mut orig = host_bits.iter().take(len).copied().collect::<Vec<_>>();
        orig.resize(110, false);
        if let Some(bit) = orig.last() {
            if *bit {
                total_bits += 1;
            }
        }
    }
    let toc = std::time::Instant::now();
    eprintln!("BitShuffling (BIAS): {:?} {total_bits}", toc - tic);
}

#[test]
fn test_bit_shuffling_extend_min_time() {
    let tic = std::time::Instant::now();
    let mut total_bits = 0;
    let host_bits = (0..200).map(|_| rand::random::<bool>()).collect::<Vec<_>>();
    for i in 0..10_000_000 {
        let len = rand::random::<usize>() % 100 + 4;
        let width = rand::random::<usize>() % len;
        let mut orig = host_bits.iter().take(len).copied().collect::<Vec<_>>();
        let pad = 110_usize.saturating_sub(len);
        orig.extend(vec![false; pad]);
        if let Some(bit) = orig.last() {
            if *bit {
                total_bits += 1;
            }
        }
    }
    let toc = std::time::Instant::now();
    eprintln!("BitShuffling (BIAS 2): {:?} {total_bits}", toc - tic);
}

#[test]
fn test_trace_with_enum_performance_serialization() {
    #[derive(Copy, Clone, PartialEq, Default, Digital, Notable)]
    enum Mixed {
        #[default]
        None,
        Bool(bool),
        Tuple(bool, Bits<3>),
        Array([bool; 3]),
        Strct {
            a: bool,
            b: Bits<3>,
        },
    }
    eprintln!("Start time with usual DB");
    let tic = std::time::Instant::now();
    let guard = note_init_db();
    for i in 0..10_000_000 {
        note_time(i * 1000);
        note_push_path("f1");
        note("empty", Mixed::None);
        note_pop_path();
        note_push_path("f2");
        note("boo", Mixed::Bool(i % 15 == 0));
        note_pop_path();
        note_push_path("f3");
        note("tup", Mixed::Tuple(i % 15 == 0, bits((i as u128) % 8)));
        note_pop_path();
        note_push_path("f4");
        note("arr", Mixed::Array([i % 15 == 0, i % 15 == 1, i % 15 == 2]));
        note_pop_path();
        note_push_path("f5");
        note(
            "strct",
            Mixed::Strct {
                a: i % 15 == 0,
                b: bits((i as u128) % 8),
            },
        );
        note_pop_path();
    }
    let toc = std::time::Instant::now();
    let mut vcd = vec![];
    guard.take().dump_vcd(&mut vcd).unwrap();
    eprintln!("Usual DB: {:?}", toc - tic);
    std::fs::write("trace_enum.vcd", vcd).unwrap();
}

#[test]
fn test_micro_benchmark() {
    let tic = std::time::Instant::now();
    let mut bigvec = vec![];
    for i in 0..10_000_000 {
        let val = i % 57;
        let mut bits = vec![];
        for ndx in 0..16 {
            if val & (1 << ndx) != 0 {
                bits.push(TraceBit::One);
            } else {
                bits.push(TraceBit::Zero);
            }
        }
        bigvec.push(bits)
    }
    let toc = std::time::Instant::now();
    eprintln!("Bigvec: {:?}", toc - tic);
    let tic = std::time::Instant::now();
    let mut bigvec = vec![];
    for i in 0..10_000_000 {
        let val = i % 57;
        let mut bits = SmallVec::<[TraceBit; 16]>::new();
        for ndx in 0..16 {
            if val & (1 << ndx) != 0 {
                bits.push(TraceBit::One);
            } else {
                bits.push(TraceBit::Zero);
            }
        }
        bigvec.push(bits)
    }
    let toc = std::time::Instant::now();
    eprintln!("Smallvec: {:?}", toc - tic);
}

#[test]
fn test_bit_vector_time() {
    let tic = std::time::Instant::now();
    let mut total_bits = 0;
    for i in 0..10_000_000 {
        let val = (i % 57) as u64;
        let mut bits = BitVector::default();
        let num_bits = if i % 1000 == 0 { 48 } else { 16 };
        for ndx in 0..num_bits {
            if val & (1 << ndx) != 0 {
                bits.push(true);
            } else {
                bits.push(false);
            }
        }
        total_bits += bits.len();
    }
    let toc = std::time::Instant::now();
    eprintln!("BitString: {:?} {total_bits}", toc - tic);
}

#[test]
fn test_fast_serialization_time() {
    #[derive(Copy, Clone, PartialEq, Default, Digital, Notable)]
    enum Mixed {
        #[default]
        None,
        Bool(bool),
        Tuple(bool, Bits<3>),
        Array([bool; 3]),
        Strct {
            a: bool,
            b: Bits<3>,
        },
    }
    let kind = Mixed::static_kind();
    let discriminant_layout = match &kind {
        Kind::Enum(kind) => kind.discriminant_layout,
        _ => panic!("Not an enum"),
    };
    let self_bits = kind.bits();
    type BitV = SmallVec<[bool; 16]>;
    //type BitV = Vec<bool>;
    let pad = |bits: BitV| -> BitV {
        let pad_len = self_bits - bits.len();
        let bits = bits.into_iter().chain(repeat(false).take(pad_len));
        match discriminant_layout.alignment {
            DiscriminantAlignment::Lsb => bits.collect(),
            DiscriminantAlignment::Msb => {
                let discriminant_width = discriminant_layout.width;
                let discriminant = bits.clone().take(discriminant_width);
                let payload = bits.skip(discriminant_width);
                payload.chain(discriminant).collect()
            }
        }
    };
    fn ser_bits<const N: usize>(value: &Bits<N>) -> BitV {
        (0..N).map(|ndx| value.0 & (1 << ndx) != 0).collect()
    }
    let ser = |value: &Mixed| -> BitV {
        //kind.pad
        pad(match value {
            Mixed::None => ser_bits(&rhdl::bits::bits::<3usize>(0i64 as u128)),
            Mixed::Bool(_0) => {
                let mut v = ser_bits(&rhdl::bits::bits::<3usize>(1i64 as u128));
                v.push(*_0);
                v
            }
            Mixed::Tuple(_0, _1) => {
                let mut v = ser_bits(&rhdl::bits::bits::<3usize>(2i64 as u128));
                v.push(*_0);
                v.extend(ser_bits(_1));
                v
            }
            Mixed::Array(_0) => {
                let mut v = ser_bits(&rhdl::bits::bits::<3usize>(3i64 as u128));
                v.extend(_0.iter().copied());
                v
            }
            Mixed::Strct { a, b } => {
                let mut v = ser_bits(&rhdl::bits::bits::<3usize>(4i64 as u128));
                v.push(*a);
                v.extend(ser_bits(b));
                v
            }
        })
    };
    let tic = std::time::Instant::now();
    let mut total_bits = 0;
    for i in 0..10_000_000 {
        total_bits += ser(&Mixed::None).len();
        total_bits += ser(&Mixed::Bool(i % 15 == 0)).len();
        total_bits += ser(&Mixed::Tuple(i % 15 == 0, bits((i as u128) % 8))).len();
        total_bits += ser(&Mixed::Array([i % 15 == 0, i % 15 == 1, i % 15 == 2])).len();
        total_bits += ser(&Mixed::Strct {
            a: i % 15 == 0,
            b: bits((i as u128) % 8),
        })
        .len();
    }
    let toc = std::time::Instant::now();
    eprintln!("Fast Serialization : {:?} {total_bits}", toc - tic);
}

#[test]
fn test_serialization_time() {
    #[derive(Copy, Clone, PartialEq, Default, Digital, Notable)]
    enum Mixed {
        #[default]
        None,
        Bool(bool),
        Tuple(bool, Bits<3>),
        Array([bool; 3]),
        Strct {
            a: bool,
            b: Bits<3>,
        },
    }
    let tic = std::time::Instant::now();
    let mut total_bits = 0;
    for i in 0..10_000_000 {
        total_bits += (&Mixed::None).bin().len();
        total_bits += (&Mixed::Bool(i % 15 == 0)).bin().len();
        total_bits += (&Mixed::Tuple(i % 15 == 0, bits((i as u128) % 8)))
            .bin()
            .len();
        total_bits += (&Mixed::Array([i % 15 == 0, i % 15 == 1, i % 15 == 2]))
            .bin()
            .len();
        total_bits += (&Mixed::Strct {
            a: i % 15 == 0,
            b: bits((i as u128) % 8),
        })
            .bin()
            .len();
    }
    let toc = std::time::Instant::now();
    eprintln!("Serialization : {:?}", toc - tic);
}

#[test]
fn test_new_tracedb_performance_serialization() {
    #[derive(Copy, Clone, PartialEq, Default, Digital, Notable)]
    enum Mixed {
        #[default]
        None,
        Bool(bool),
        Tuple(bool, Bits<3>),
        Array([bool; 3]),
        Strct {
            a: bool,
            b: Bits<3>,
        },
    }
    let tic = std::time::Instant::now();
    let mut db = TraceDB::default();
    for i in 0..10_000_000 {
        db.set_time(i * 1000);
        db.push_path("f1");
        db.trace("mpty", &Mixed::None);
        db.pop_path();
        db.push_path("f2");
        db.trace("boo", &Mixed::Bool(i % 15 == 0));
        db.pop_path();
        db.push_path("f3");
        db.trace("tup", &Mixed::Tuple(i % 15 == 0, bits((i as u128) % 8)));
        db.pop_path();
        db.push_path("f4");
        db.trace(
            "arr",
            &Mixed::Array([i % 15 == 0, i % 15 == 1, i % 15 == 2]),
        );
        db.pop_path();
        db.push_path("f5");
        db.trace(
            "strct",
            &Mixed::Strct {
                a: i % 15 == 0,
                b: bits((i as u128) % 8),
            },
        );
        db.pop_path();
    }
    let toc = std::time::Instant::now();
    eprintln!("TraceDB: {:?}", toc - tic);
    let mut vcd = vec![];
    db.dump_vcd(&mut vcd).unwrap();
    let tac = std::time::Instant::now();
    eprintln!("VCD Dump: {:?}", tac - toc);
    std::fs::write("trace_enum_trace_new.vcd", vcd).unwrap();
}
