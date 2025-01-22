use rhdl::{
    core::hdl::ast::{index, unsigned_reg_decl, unsigned_wire_decl},
    prelude::*,
};

/// A simple two-register synchronizer for crossing
/// a single bit from the W domain to the R domain
#[derive(PartialEq, Debug, Clone, Default)]
pub struct U<W: Domain, R: Domain> {
    _w: std::marker::PhantomData<W>,
    _r: std::marker::PhantomData<R>,
}

#[derive(PartialEq, Debug, Digital, Timed)]
pub struct I<W: Domain, R: Domain> {
    pub data: Signal<bool, W>,
    pub cr: Signal<ClockReset, R>,
}

impl<W: Domain, R: Domain> CircuitDQ for U<W, R> {
    type D = ();
    type Q = ();
}

impl<W: Domain, R: Domain> CircuitIO for U<W, R> {
    type I = I<W, R>;
    type O = Signal<bool, R>;
    type Kernel = NoKernel2<Self::I, (), (Self::O, ())>;
}

#[derive(PartialEq, Debug, Digital)]
pub struct S {
    clock: Clock,
    reg1_next: bool,
    reg1_current: bool,
    reg2_next: bool,
    reg2_current: bool,
}

impl<W: Domain, R: Domain> Circuit for U<W, R> {
    type S = S;

    fn init(&self) -> Self::S {
        S {
            clock: Clock::dont_care(),
            reg1_next: false,
            reg1_current: false,
            reg2_next: false,
            reg2_current: false,
        }
    }

    fn description(&self) -> String {
        format!("Synchronizer from {:?}->{:?}", W::color(), R::color())
    }

    fn sim(&self, input: Self::I, state: &mut Self::S) -> Self::O {
        let clock = input.cr.val().clock;
        let reset = input.cr.val().reset;
        trace("clock", &clock);
        trace("reset", &reset);
        trace("input", &input.data);
        if !clock.raw() {
            state.reg1_next = input.data.val();
            state.reg2_next = state.reg1_current;
        }
        if clock.raw() && !state.clock.raw() {
            state.reg1_current = state.reg1_next;
            state.reg2_current = state.reg2_next;
        }
        if reset.raw() {
            state.reg1_next = false;
            state.reg2_next = false;
        }
        state.clock = clock;
        trace("output", &state.reg2_current);
        signal(state.reg2_current)
    }

    fn descriptor(&self, name: &str) -> Result<CircuitDescriptor, RHDLError> {
        let mut flow_graph = FlowGraph::default();
        let hdl = self.hdl(&format!("{name}_inner"))?;
        let (input, output) = flow_graph.circuit_black_box::<Self>(hdl);
        flow_graph.inputs = vec![input];
        flow_graph.output = output;
        Ok(CircuitDescriptor {
            unique_name: name.to_string(),
            flow_graph,
            input_kind: <Self::I as Timed>::static_kind(),
            output_kind: <Self::O as Timed>::static_kind(),
            d_kind: Kind::Empty,
            q_kind: Kind::Empty,
            children: Default::default(),
            rtl: None,
        })
    }

    fn hdl(&self, name: &str) -> Result<HDLDescriptor, RHDLError> {
        let module_name = name.to_owned();
        let mut module = Module {
            name: module_name.clone(),
            ..Default::default()
        };
        let i_kind = <Self::I as Timed>::static_kind();
        module.ports = vec![
            port("i", Direction::Input, HDLKind::Wire, unsigned_width(3)),
            port("o", Direction::Output, HDLKind::Wire, unsigned_width(1)),
        ];
        module.declarations.extend([
            unsigned_wire_decl("data", 1),
            unsigned_wire_decl("clock", 1),
            unsigned_wire_decl("reset", 1),
            unsigned_reg_decl("reg1", 1),
            unsigned_reg_decl("reg2", 1),
        ]);
        let reassign = |name: &str, path: Path| {
            continuous_assignment(name, index("i", bit_range(i_kind, &path).unwrap().0))
        };
        module.statements.extend([
            reassign("data", Path::default().field("data").signal_value()),
            reassign(
                "clock",
                Path::default().field("cr").signal_value().field("clock"),
            ),
            reassign(
                "reset",
                Path::default().field("cr").signal_value().field("reset"),
            ),
            continuous_assignment("o", id("reg2")),
        ]);
        let init = false.typed_bits().into();
        let reg1 = if_statement(
            id("reset"),
            vec![non_blocking_assignment("reg1", bit_string(&init))],
            vec![non_blocking_assignment("reg1", id("data"))],
        );
        let reg2 = if_statement(
            id("reset"),
            vec![non_blocking_assignment("reg2", bit_string(&init))],
            vec![non_blocking_assignment("reg2", id("reg1"))],
        );
        let events = vec![Events::Posedge("clock".into())];
        module.statements.push(always(events, vec![reg1, reg2]));
        Ok(HDLDescriptor {
            name: name.into(),
            body: module,
            children: Default::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use expect_test::expect;
    use rand::{Rng, SeedableRng};
    use rhdl::core::sim::vcd;

    use super::*;

    fn sync_stream() -> impl Iterator<Item = TimedSample<I<Red, Blue>>> {
        let mut rng = rand::rngs::StdRng::seed_from_u64(0xdead_beef);
        // Assume the red stuff comes on the edges of a clock
        let red = (0..)
            .map(move |_| rng.gen::<bool>())
            .take(100)
            .stream_after_reset(1)
            .clock_pos_edge(100);
        let blue = std::iter::repeat(false)
            .stream_after_reset(1)
            .clock_pos_edge(79);
        red.merge(blue, |r, g| I {
            data: signal(r.1),
            cr: signal(g.0),
        })
    }

    #[test]
    fn test_sync_stream_makes_sense() -> miette::Result<()> {
        let stream = sync_stream();
        for (ndx, val) in stream
            .take(150)
            .edge_time(|p| p.value.cr.val().clock)
            .filter(|x| x.value.cr.val().clock.raw())
            .enumerate()
        {
            let pred = 39 + 78 * ndx;
            assert!(pred == val.time as usize);
        }
        let stream = sync_stream();
        for (ndx, val) in stream
            .take(150)
            .edge_time(|p| p.value.cr.val().clock)
            .filter(|x| !x.value.cr.val().clock.raw())
            .enumerate()
        {
            let pred = 78 + 78 * ndx;
            assert!(pred == val.time as usize);
        }
        Ok(())
    }

    #[test]
    fn test_hdl_generation() -> miette::Result<()> {
        let uut = U::<Red, Blue>::default();
        let stream = sync_stream();
        let test_bench = uut.run(stream)?.collect::<TestBench<_, _>>();
        let test_mod =
            test_bench.rtl(&uut, &TestBenchOptions::default().vcd("hdl.vcd").skip(!0))?;
        std::fs::write("synchronizer.v", test_mod.to_string()).unwrap();
        test_mod.run_iverilog()?;
        Ok(())
    }

    #[test]
    fn test_synchronizer_performance() -> miette::Result<()> {
        let uut = U::<Red, Blue>::default();
        // Assume the Blue stuff comes on the edges of a clock
        let input = sync_stream();
        let _ = uut
            .run(input)?
            .glitch_check(|i| (i.value.0.cr.val().clock, i.value.1.val()))
            .last();
        Ok(())
    }

    #[test]
    fn test_synchronizer_function() -> miette::Result<()> {
        let uut = U::<Red, Blue>::default();
        let input = sync_stream();
        let vcd = uut.run(input)?.collect::<vcd::Vcd>();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("vcd")
            .join("synchronizer");
        std::fs::create_dir_all(&root).unwrap();
        let expect = expect!["6447afec090e6d976ed27898d22bfef13361bff6b78b6dbc7db1ada3bcd29252"];
        let digest = vcd.dump_to_file(&root.join("synchronizer.vcd")).unwrap();
        expect.assert_eq(&digest);
        Ok(())
    }
}
