use std::collections::HashMap;
use std::sync::Arc;

use chrono::{FixedOffset, NaiveDate, Timelike, Utc};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::models::settings::SchedulerSettings;
use crate::services::jobs::JobService;
use crate::services::settings::SettingsService;

const TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Runs the scheduler tick loop until `shutdown_rx` fires.
///
/// Checks every 30 seconds whether a scheduled job should fire based on
/// the current KST time and the configured schedule in settings.json.
pub async fn run_scheduler_loop(
    job_service: Arc<JobService>,
    settings_service: Arc<SettingsService>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    info!("scheduler loop started");

    let mut last_fired: HashMap<&str, (NaiveDate, u8, u8)> = HashMap::new();
    let kst = FixedOffset::east_opt(9 * 3600).expect("valid KST offset");

    let mut interval = tokio::time::interval(TICK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown_rx.changed() => {
                info!("scheduler loop shutting down");
                break;
            }
        }

        let settings = match settings_service.get_settings().await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "scheduler: failed to read settings, skipping tick");
                continue;
            }
        };

        if !settings.scheduler.enabled {
            continue;
        }

        let now_kst = Utc::now().with_timezone(&kst);
        let today_kst = now_kst.date_naive();
        let current_hour = now_kst.hour() as u8;
        let current_minute = now_kst.minute() as u8;

        for (source, job_id) in [
            ("arxiv", "arxiv_daily"),
            ("pmc", "pmc_daily"),
            ("pubmed", "pubmed_daily"),
        ] {
            let (sched_hour, sched_minute) = extract_schedule(&settings.scheduler, source);

            if current_hour != sched_hour || current_minute != sched_minute {
                continue;
            }

            if last_fired.get(source) == Some(&(today_kst, sched_hour, sched_minute)) {
                continue;
            }

            info!(
                job_id,
                source,
                time_kst = %now_kst.format("%Y-%m-%d %H:%M:%S KST"),
                "scheduler: triggering scheduled job"
            );

            match job_service.enqueue_source(source, 2).await {
                Ok(run) => {
                    info!(
                        job_id,
                        run_id = %run.run_id,
                        "scheduler: job enqueued successfully"
                    );
                }
                Err(e) => {
                    warn!(
                        job_id,
                        error = %e,
                        "scheduler: failed to enqueue job"
                    );
                }
            }

            last_fired.insert(source, (today_kst, sched_hour, sched_minute));
        }

        // Prune stale entries.
        last_fired.retain(|_, (date, _, _)| *date >= today_kst);
    }

    info!("scheduler loop exited");
}

fn extract_schedule(sched: &SchedulerSettings, source: &str) -> (u8, u8) {
    match source {
        "arxiv" => (sched.arxiv_schedule_hour, sched.arxiv_schedule_minute),
        "pmc" => (sched.pmc_schedule_hour, sched.pmc_schedule_minute),
        "pubmed" => (sched.pubmed_schedule_hour, sched.pubmed_schedule_minute),
        _ => (255, 255),
    }
}
