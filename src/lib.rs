pub mod adapter;
pub mod bundle;
pub mod http;
pub mod transit_model;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/kemana.bundle.v1.rs"));
}
