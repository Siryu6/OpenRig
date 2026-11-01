//! Time the effect chain offline, period by period, as the audio thread
//! runs it: `pedalboard-bench <model.nam> [rig N]`.
//!
//! Rigs (each block plays the model):
//! - `serial N`: N blocks in a row;
//! - `split N`: a Mix split with N blocks in each branch;
//! - `splits N`: N Mix splits in a row, one block per branch.
//!
//! `PERIOD` sets the period (128 frames), `SECONDS` the length (10 s),
//! `WORKERS` the DSP workers besides this thread (0: this thread alone),
//! `STAGE_LOAD` the share of the period before one more core (0.25),
//! `RT` a SCHED_FIFO priority to run at, as the JACK thread does (needs
//! the right to, e.g. under `prlimit --rtprio=95`).
//! Prints the time per period (mean, 99th percentile, worst) and the load:
//! the share of the period it takes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};

use pedalboard::audio::split_chain::{ChainNode, SplitType};
use pedalboard::plugins::scanner::NAM_URI;
use pedalboard::plugins::{ChainSpec, EffectChain, Lv2Host};

const RATE: f32 = 48000.0;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let model = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow!("usage: pedalboard-bench <model.nam> [rig N]"))?,
    );
    let rig = args.next().unwrap_or_else(|| "serial".into());
    let count: usize = match args.next() {
        Some(n) => n.parse().context("block count")?,
        None => 2,
    };
    let period: usize = env("PERIOD", 128);
    let seconds: usize = env("SECONDS", 10);
    // The input below is one second long
    if !(1..=RATE as usize / 2).contains(&period) {
        bail!("PERIOD must be between 1 and {} frames", RATE as usize / 2);
    }
    if !(1..=3600).contains(&seconds) {
        bail!("SECONDS must be between 1 and 3600");
    }

    let mut next_id = 0;
    let mut block = || {
        next_id += 1;
        ChainNode::Plugin {
            instance_id: next_id,
            uri: NAM_URI.to_string(),
            enabled: true,
            num_inputs: 1,
            num_outputs: 1,
        }
    };
    let split = |top: Vec<ChainNode>, bottom: Vec<ChainNode>, id: usize| ChainNode::Split {
        instance_id: 1000 + id,
        split_type: SplitType::Mix,
        enabled: true,
        top_chain: top,
        bottom_chain: bottom,
        ab_select_a: true,
        mix_ratio: 0.5,
        pan: [0.0; 2],
        num_inputs: 1,
        num_outputs: 1,
    };
    let nodes = match rig.as_str() {
        "serial" => (0..count).map(|_| block()).collect(),
        "split" => {
            let top = (0..count).map(|_| block()).collect();
            let bottom = (0..count).map(|_| block()).collect();
            vec![split(top, bottom, 0)]
        }
        "splits" => (0..count)
            .map(|k| {
                let (top, bottom) = (vec![block()], vec![block()]);
                split(top, bottom, k)
            })
            .collect(),
        other => bail!("unknown rig {other}"),
    };
    let mut files = HashMap::new();
    collect_ids(&nodes, &mut |id| {
        files.insert(id, model.clone());
    });
    let spec = ChainSpec {
        nodes,
        files,
        ..ChainSpec::default()
    };

    let host = Lv2Host::new().context("LV2 host")?;
    let (mut chain, errors) = EffectChain::build(&spec, &host, RATE, period);
    if !errors.is_empty() {
        bail!("chain not built: {errors:?}");
    }
    println!("{rig} {count}: {} blocks, {period} frames", chain.len());

    let workers = env("WORKERS", 0);
    if let Some(share) = std::env::var("STAGE_LOAD")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        pedalboard::plugins::chain::set_stage_load(share);
    }
    if let Some(priority) = std::env::var("RT").ok().and_then(|v| v.parse().ok()) {
        real_time(priority)?;
    }
    pedalboard::audio::dsp_pool::start(workers);
    pedalboard::audio::flush_denormals();
    // A guitar-like input: a decaying pluck every half second
    let input: Vec<f32> = (0..RATE as usize)
        .map(|i| {
            let t = (i % 24000) as f32 / RATE;
            0.3 * (-6.0 * t).exp() * (2.0 * std::f32::consts::PI * 110.0 * t).sin()
        })
        .collect();
    let (mut left, mut right) = (vec![0.0f32; period], vec![0.0f32; period]);
    let periods = seconds * RATE as usize / period;
    let mut times = Vec::with_capacity(periods);
    let mut cpu = 0.0;
    for k in 0..periods + 100 {
        let at = k * period % (input.len() - period);
        left.copy_from_slice(&input[at..at + period]);
        let (start, start_cpu) = (Instant::now(), thread_cpu());
        chain.process(&mut left, &mut right);
        // The first periods warm the caches up
        if k >= 100 {
            times.push(start.elapsed().as_secs_f64() * 1e6);
            cpu += thread_cpu() - start_cpu;
        }
    }
    times.sort_by(f64::total_cmp);
    let budget = period as f64 / RATE as f64 * 1e6;
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let p99 = times
        .get(times.len() * 99 / 100)
        .copied()
        .unwrap_or_default();
    let worst = times.last().copied().unwrap_or_default();
    println!(
        "{workers} worker(s), {} jobs: per period: mean {mean:.0} µs, p99 {p99:.0} µs, worst {worst:.0} µs \
         (budget {budget:.0} µs) → load {:.0} % mean, {:.0} % p99",
        pedalboard::audio::dsp_pool::jobs_run(),
        100.0 * mean / budget,
        100.0 * p99 / budget
    );
    // Unlike the wall clock, not stretched by other programs: compares
    // builds on a busy machine (with no workers)
    println!(
        "this thread's CPU time: mean {:.0} µs per period",
        cpu / times.len() as f64
    );
    Ok(())
}

fn collect_ids(nodes: &[ChainNode], f: &mut impl FnMut(usize)) {
    for node in nodes {
        match node {
            ChainNode::Plugin { instance_id, .. } => f(*instance_id),
            ChainNode::Split {
                top_chain,
                bottom_chain,
                ..
            } => {
                collect_ids(top_chain, f);
                collect_ids(bottom_chain, f);
            }
        }
    }
}

/// CPU time of this thread, in µs
#[cfg(unix)]
fn thread_cpu() -> f64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: an out-parameter valid for the call
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    t.tv_sec as f64 * 1e6 + t.tv_nsec as f64 / 1e3
}

#[cfg(not(unix))]
fn thread_cpu() -> f64 {
    0.0
}

/// Run this thread (and so the DSP workers) with SCHED_FIFO at `priority`
#[cfg(unix)]
fn real_time(priority: i32) -> Result<()> {
    // SAFETY: a plain C struct, all zeroes is a valid value
    let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
    param.sched_priority = priority;
    // SAFETY: the calling thread, and an in-parameter valid for the call
    let err =
        unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) };
    if err != 0 {
        bail!(
            "SCHED_FIFO {priority}: {}",
            std::io::Error::from_raw_os_error(err)
        );
    }
    println!("SCHED_FIFO {priority}");
    Ok(())
}

#[cfg(not(unix))]
fn real_time(_priority: i32) -> Result<()> {
    bail!("RT: no SCHED_FIFO on this platform")
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
