//! Every audiod integration test as one binary, so it links once. A new test
//! file needs its `mod` here; `include_str!` paths climb one extra level.

mod beat_relay;
mod capture;
mod capture_argv;
mod events;
mod ingest;
mod meter;
mod pause;
mod pause_mirror;
mod rebase;
mod segmenter;
mod upload;
mod upload_real_server;
mod usage;
mod wire;
mod wire_fixture;
