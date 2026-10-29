//! The gated operations of the DataForSEO adapter: `estimate`/`_preflight`/`_call`/`_run`
//! cost plumbing, the four Google wrappers and the opt-in backlink calls.
//!
//! Every wrapper follows the same shape: `geo_guard` → payload → `_preflight`
//! (confirmation + spend ceiling before any network) → client → `_run` → result envelope.

use serde_json::{json, Map, Value};

use super::{
    attr_err, endpoints, geo_guard, py_iter, pyerr, task_errors, task_items, tasks_all_failed,
    type_err, value_err, Client, ClientArgs, DfsEnv, DfsError, SOURCE,
};
use crate::http::Transport;
use crate::spend::Record;
use crate::spend_gate;
use crate::{pystr, pytime};

mod helpers;
pub use helpers::*;
mod estimate;
pub use estimate::*;
mod run;
pub use run::*;
mod keywords;
pub use keywords::*;
mod backlinks;
pub use backlinks::*;
