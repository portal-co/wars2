use std::error::Error;
use std::fmt::Display;

use tester::*;
use wars_rt::_rexport::tramp::tramp;
use wars_rt::_rexport::tuple_list::tuple_list;
#[derive(Debug)]
enum HostError {}
impl Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {}
    }
}
impl Error for HostError {}

// --- Waffle ---
struct HostWaffle {
    data: waffle_generated::TestModuleData<HostWaffle>,
}

impl wars_rt::CtxSpec for HostWaffle {
    type ExternRef = ();
    type Error = HostError;
}
impl waffle_generated::TestModule for HostWaffle {
    type _ExternRef = ();
    type _Error = HostError;
    fn data(&mut self) -> &mut waffle_generated::TestModuleData<Self> {
        &mut self.data
    }
}

#[test]
fn test_waffle_add() {
    use waffle_generated::TestModuleImpl;
    let mut host = HostWaffle {
        data: Default::default(),
    };
    host.init().unwrap();
    let res: Result<(u32, ()), HostError> = tramp(host.add(tuple_list!(10u32, 20u32)));
    assert_eq!(res.unwrap(), tuple_list!(30u32));
}

#[test]
fn test_waffle_global() {
    use waffle_generated::TestModuleImpl;
    let mut host = HostWaffle {
        data: Default::default(),
    };
    host.init().unwrap();
    let res: Result<(u32, ()), HostError> = tramp(host.getglobal(tuple_list!()));
    assert_eq!(res.unwrap(), tuple_list!(42u32));

    let _: Result<(), HostError> = tramp(host.setglobal(tuple_list!(100u32)));
    let res: Result<(u32, ()), HostError> = tramp(host.getglobal(tuple_list!()));
    assert_eq!(res.unwrap(), tuple_list!(100u32));
}

#[test]
fn test_waffle_memory() {
    use waffle_generated::TestModuleImpl;
    let mut host = HostWaffle {
        data: Default::default(),
    };
    host.init().unwrap();

    let _: Result<(), HostError> = tramp(host.storei32(tuple_list!(0u32, 123456u32)));
    let res: Result<(u32, ()), HostError> = tramp(host.loadi32(tuple_list!(0u32)));
    assert_eq!(res.unwrap(), tuple_list!(123456u32));
}

#[test]
fn test_waffle_calladd() {
    use waffle_generated::TestModuleImpl;
    let mut host = HostWaffle {
        data: Default::default(),
    };
    host.init().unwrap();
    let res: Result<(u32, ()), HostError> = tramp(host.calladd(tuple_list!(10u32, 20u32)));
    assert_eq!(res.unwrap(), tuple_list!(30u32));
}

// --- Wasmparser ---
struct HostWp {
    data: wp_generated::TestModuleData<HostWp>,
}
impl wars_rt::CtxSpec for HostWp {
    type ExternRef = ();
    type Error = HostError;
}
impl wp_generated::TestModule for HostWp {
    type _ExternRef = ();
    type _Error = HostError;
    fn data(&mut self) -> &mut wp_generated::TestModuleData<Self> {
        &mut self.data
    }
}

#[test]
fn test_wp_add() {
    use wp_generated::TestModuleImpl;
    let mut host = HostWp {
        data: Default::default(),
    };
    host.init().unwrap();
    let res: Result<(u32, ()), HostError> = tramp(host.add(tuple_list!(10u32, 20u32)));
    assert_eq!(res.unwrap(), tuple_list!(30u32));
}

#[test]
fn test_wp_global() {
    use wp_generated::TestModuleImpl;
    let mut host = HostWp {
        data: Default::default(),
    };
    host.init().unwrap();
    let res: Result<(u32, ()), HostError> = tramp(host.getglobal(tuple_list!()));
    assert_eq!(res.unwrap(), tuple_list!(42u32));

    let _: Result<(), HostError> = tramp(host.setglobal(tuple_list!(100u32)));
    let res: Result<(u32, ()), HostError> = tramp(host.getglobal(tuple_list!()));
    assert_eq!(res.unwrap(), tuple_list!(100u32));
}

#[test]
fn test_wp_memory() {
    use wp_generated::TestModuleImpl;
    let mut host = HostWp {
        data: Default::default(),
    };
    host.init().unwrap();

    let _: Result<(), HostError> = tramp(host.storei32(tuple_list!(0u32, 123456u32)));
    let res: Result<(u32, ()), HostError> = tramp(host.loadi32(tuple_list!(0u32)));
    assert_eq!(res.unwrap(), tuple_list!(123456u32));
}

#[test]
fn test_wp_calladd() {
    use wp_generated::TestModuleImpl;
    let mut host = HostWp {
        data: Default::default(),
    };
    host.init().unwrap();
    let res: Result<(u32, ()), HostError> = tramp(host.calladd(tuple_list!(10u32, 20u32)));
    assert_eq!(res.unwrap(), tuple_list!(30u32));
}
