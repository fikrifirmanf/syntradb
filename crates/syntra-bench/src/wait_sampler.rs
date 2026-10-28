use crate::mixed::Gate;
use postgres::Client;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use syntra_bench::{ActivityObservation, ActivitySample, Result, WaitSamples};

pub(super) struct WaitSampler {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<std::result::Result<WaitSamples, String>>>,
}

impl WaitSampler {
    pub fn start(
        mut client: Client,
        pids: Vec<i32>,
        interval_ms: u64,
        gate: Arc<Gate>,
    ) -> Result<Self> {
        if pids.is_empty() || pids.len() > 16 || !(5..=1000).contains(&interval_ms) {
            return Err("invalid backend sampler configuration".into());
        }
        client.batch_execute(
            "SET statement_timeout = '2s'; SET application_name = 'syntra_wait_sampler'",
        )?;
        let statement = client.prepare("SELECT pid, state, wait_event_type, wait_event FROM pg_stat_activity WHERE pid = ANY($1::int4[]) ORDER BY pid")?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let handle = thread::Builder::new()
            .name("syntra-wait-sampler".to_owned())
            .spawn(move || {
                let mut run = || -> Result<WaitSamples> {
                    let mut samples = WaitSamples::new(7000)?;
                    let Some(start) = gate.wait()? else {
                        return Ok(samples);
                    };
                    let interval = Duration::from_millis(interval_ms);
                    let mut next = start;
                    while !stopping.load(Ordering::Relaxed) {
                        if let Some(delay) = next.checked_duration_since(Instant::now()) {
                            thread::sleep(delay);
                        }
                        if stopping.load(Ordering::Relaxed) {
                            break;
                        }
                        let sent = Instant::now();
                        let rows = client.query(&statement, &[&pids])?;
                        let received = Instant::now();
                        let observations = rows
                            .iter()
                            .map(|row| {
                                Ok(ActivityObservation {
                                    pid: row.try_get(0)?,
                                    state: row.try_get(1)?,
                                    wait_type: row.try_get(2)?,
                                    wait_event: row.try_get(3)?,
                                })
                            })
                            .collect::<Result<Vec<_>>>()?;
                        samples.record(ActivitySample {
                            sent_us: sent.saturating_duration_since(start).as_micros() as u64,
                            received_us: received.saturating_duration_since(start).as_micros()
                                as u64,
                            observations,
                        })?;
                        next += interval;
                        let now = Instant::now();
                        if next < now {
                            let missed =
                                (now.duration_since(next).as_millis() as u64 / interval_ms) + 1;
                            samples.skip_ticks(missed);
                            next += interval * u32::try_from(missed)?;
                        }
                    }
                    Ok(samples)
                };
                run().map_err(|error| error.to_string())
            })?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }

    pub fn finish(mut self) -> Result<WaitSamples> {
        self.stop.store(true, Ordering::Relaxed);
        let handle = self.handle.take().ok_or("sampler already joined")?;
        handle
            .join()
            .map_err(|_| "backend sampler panicked")?
            .map_err(Into::into)
    }
}

impl Drop for WaitSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
