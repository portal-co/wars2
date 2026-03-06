use tester::*;
use wars_rt::_rexport::tuple_list::tuple_list;
use wars_rt::_rexport::tramp::tramp;

// --- Waffle ---
struct HostWaffle {
    data: waffle_generated::TestModuleData<HostWaffle>,
}
impl wars_rt::CtxSpec for HostWaffle {
    type ExternRef = ();
}
impl waffle_generated::TestModule for HostWaffle {
    type _ExternRef = ();
    fn data(&mut self) -> &mut waffle_generated::TestModuleData<Self> {
        &mut self.data
    }
}

static_assertions::assert_impl_all!(HostWaffle: waffle_generated::TestModule, waffle_generated::TestModuleImpl);

#[test]
fn test_waffle_add() {
    use waffle_generated::TestModuleImpl;
    let mut host = HostWaffle { data: Default::default() };
    host.init().unwrap();
    let res: anyhow::Result<(u32, ())> = tramp(host.add(tuple_list!(10u32, 20u32)));
    assert_eq!(res.unwrap(), tuple_list!(30u32));
}

#[test]
fn test_waffle_global() {
    use waffle_generated::TestModuleImpl;
    let mut host = HostWaffle { data: Default::default() };
    host.init().unwrap();
    let res: anyhow::Result<(u32, ())> = tramp(host.getglobal(tuple_list!()));
    assert_eq!(res.unwrap(), tuple_list!(42u32));
    
    let _: anyhow::Result<()> = tramp(host.setglobal(tuple_list!(100u32)));
    let res: anyhow::Result<(u32, ())> = tramp(host.getglobal(tuple_list!()));
    assert_eq!(res.unwrap(), tuple_list!(100u32));
}

#[test]
fn test_waffle_memory() {
    use waffle_generated::TestModuleImpl;
    let mut host = HostWaffle { data: Default::default() };
    host.init().unwrap();
    
    let _: anyhow::Result<()> = tramp(host.storei32(tuple_list!(0u32, 123456u32)));
    let res: anyhow::Result<(u32, ())> = tramp(host.loadi32(tuple_list!(0u32)));
    assert_eq!(res.unwrap(), tuple_list!(123456u32));
}

#[test]
fn test_waffle_calladd() {
    use waffle_generated::TestModuleImpl;
    let mut host = HostWaffle { data: Default::default() };
    host.init().unwrap();
    let res: anyhow::Result<(u32, ())> = tramp(host.calladd(tuple_list!(10u32, 20u32)));
    assert_eq!(res.unwrap(), tuple_list!(30u32));
}

// --- Wasmparser ---
struct HostWp {
    data: wp_generated::TestModuleData<HostWp>,
}
impl wars_rt::CtxSpec for HostWp {
    type ExternRef = ();
}
impl wp_generated::TestModule for HostWp {
    type _ExternRef = ();
    fn data(&mut self) -> &mut wp_generated::TestModuleData<Self> {
        &mut self.data
    }
}

static_assertions::assert_impl_all!(HostWp: wp_generated::TestModule, wp_generated::TestModuleImpl);

#[test]
fn test_wp_add() {
    use wp_generated::TestModuleImpl;
    let mut host = HostWp { data: Default::default() };
    host.init().unwrap();
    let res: anyhow::Result<(u32, ())> = tramp(host.add(tuple_list!(10u32, 20u32)));
    assert_eq!(res.unwrap(), tuple_list!(30u32));
}

#[test]
fn test_wp_global() {
    use wp_generated::TestModuleImpl;
    let mut host = HostWp { data: Default::default() };
    host.init().unwrap();
    let res: anyhow::Result<(u32, ())> = tramp(host.getglobal(tuple_list!()));
    assert_eq!(res.unwrap(), tuple_list!(42u32));
    
    let _: anyhow::Result<()> = tramp(host.setglobal(tuple_list!(100u32)));
    let res: anyhow::Result<(u32, ())> = tramp(host.getglobal(tuple_list!()));
    assert_eq!(res.unwrap(), tuple_list!(100u32));
}

#[test]
fn test_wp_memory() {
    use wp_generated::TestModuleImpl;
    let mut host = HostWp { data: Default::default() };
    host.init().unwrap();
    
    let _: anyhow::Result<()> = tramp(host.storei32(tuple_list!(0u32, 123456u32)));
    let res: anyhow::Result<(u32, ())> = tramp(host.loadi32(tuple_list!(0u32)));
    assert_eq!(res.unwrap(), tuple_list!(123456u32));
}

#[test]
fn test_wp_calladd() {
    use wp_generated::TestModuleImpl;
    let mut host = HostWp { data: Default::default() };
    host.init().unwrap();
    let res: anyhow::Result<(u32, ())> = tramp(host.calladd(tuple_list!(10u32, 20u32)));
    assert_eq!(res.unwrap(), tuple_list!(30u32));
}
