#![allow(dead_code)]

// We need wars_rt to be available as ::wars_rt in the generated code.
extern crate wars_rt;

pub mod waffle_generated {
    include!(concat!(env!("OUT_DIR"), "/generated_waffle.rs"));
}

pub mod wp_generated {
    include!(concat!(env!("OUT_DIR"), "/generated_wp.rs"));
}
