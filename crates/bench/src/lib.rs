//! `vayu-bench`: the benchmark harness behind the project's speed claims
//! (MASTER-PLAN §1 item 10, §8 Phase 0 gate, §12).
//!
//! It runs download managers ([`adapter`]) against `vayu-labserver`
//! scenarios ([`lab`]), measures each run ([`measure`]: wall time, CPU,
//! peak RSS; labserver adds time to first byte and peak connections),
//! verifies every byte, and keeps the raw data ([`runner`]). [`report`]
//! turns a run set into Markdown with medians, p90s and Mann-Whitney U
//! tests ([`stats`]). [`fetch`] installs pinned competitor builds from
//! their official GitHub releases, verified against published checksums.
//! [`hardkill`] is the crash test: SIGKILL the real daemon mid-download,
//! restart it, and verify every finished file.

pub mod adapter;
pub mod fetch;
pub mod gate;
pub mod hardkill;
pub mod lab;
pub mod measure;
pub mod names;
pub mod report;
pub mod runner;
pub mod stats;
pub mod sys;
