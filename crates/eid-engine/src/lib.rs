//! Eid Download engine: segmented HTTP downloading with resume.
//!
//! Safety properties (each has named tests, see docs/PLAN.md):
//! * bytes are only ever written inside the segment a worker owns (reservation under lock,
//!   ADR-0009);
//! * persisted progress only advances after the data was `sync_data`-ed;
//! * file names / paths from the network are sanitized and confined to the download dir;
//! * secrets (URL credentials, query tokens) never reach state files, logs or errors.

pub mod batch;
pub mod category;
pub mod config;
pub mod download;
pub mod error;
pub mod fault;
pub mod filename;
pub mod grabber;
pub mod http;
pub mod limiter;
pub mod links;
pub mod porting;
pub mod prefix_hash;
pub mod queue;
pub mod queue_runner;
pub mod sanitize;
pub mod scheduler;
pub mod state;
pub mod store;
pub mod table;
pub mod urlsafe;

pub use config::Config;
#[doc(hidden)]
pub use download::JobLock; // exposed for the lock-race test only
pub use download::{download, resume, Event, EventSink, JobSpec, Outcome};
pub use error::{Error, Result};
pub use limiter::Limiter;
pub use tokio_util::sync::CancellationToken;
