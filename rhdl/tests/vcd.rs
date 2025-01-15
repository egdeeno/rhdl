#![allow(unused_variables)]
#![allow(unused_assignments)]
#![allow(unused_mut)]
#![allow(unreachable_code)]
#![allow(unused_must_use)]
#![allow(dead_code)]
use rhdl::prelude::*;

#[test]
fn test_vcd_enum() {
    #[derive(PartialEq, Debug, Digital, Default)]
    enum Enum {
        #[default]
        None,
        A(b8, b16),
        B {
            name: b8,
        },
        C(bool),
    }

    let guard = trace_init_db();
    trace_time(0);
    trace("enum", &Enum::None);
    trace("color", &b8(0b10101010));
    trace_time(1_000);
    trace("enum", &Enum::A(bits(42), bits(1024)));
    trace_time(2_000);
    trace("enum", &Enum::B { name: bits(67) });
    trace_time(3_000);
    trace("enum", &Enum::C(true));
    trace_time(4_000);
    trace("enum", &Enum::C(false));
    trace_time(5_000);
    trace("enum", &Enum::B { name: bits(65) });
    trace_time(6_000);
    trace("enum", &Enum::A(bits(21), bits(512)));
    trace_time(7_000);
    trace("enum", &Enum::None);
    trace_time(8_000);
    trace("enum", &Enum::None);
    let mut vcd_file = std::fs::File::create("test_enum.vcd").unwrap();
    guard.take().dump_vcd(&mut vcd_file, None).unwrap();
}

#[test]
fn test_vcd_basic() {
    #[derive(PartialEq, Digital)]
    pub struct Simple {
        a: bool,
        b: Bits<U8>,
    }

    let simple = Simple {
        a: true,
        b: Bits::from(0b10101010),
    };
    let guard = trace_init_db();
    trace_time(0);
    trace("simple", &simple);
    trace_time(1_000);
    let simple = Simple {
        a: false,
        b: Bits::from(0b01010101),
    };
    let mut snapshot = std::fs::File::create("snapshot.vcd").unwrap();
    with_trace_db(|db| db.dump_vcd(&mut snapshot, None).unwrap());
    trace("simple", &simple);
    trace_time(2_000);
    trace("simple", &simple);
    let mut vcd_file = std::fs::File::create("test.vcd").unwrap();
    guard.take().dump_vcd(&mut vcd_file, None).unwrap();
}
