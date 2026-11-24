//! Keeps the presence service in step with what the sensors say.

use std::time::Duration;

use chatsvc::{ActivityReporting, PresenceService, PresenceStatus};
use tern_presence::Desired;
use tokio::time::Instant;

/// Push down for this long: report activity over HTTP instead.
const DIRECT_AFTER: Duration = Duration::from_secs(60);
/// Direct reports keep the endpoint alive.
const DIRECT_EVERY: Duration = Duration::from_secs(120);
const RETRY_FIRST: Duration = Duration::from_secs(10);
const RETRY_MAX: Duration = Duration::from_secs(300);

/// The calls the link makes, replaced in tests.
#[allow(async_fn_in_trait)]
pub trait Service {
    async fn register(&self, endpoint: &str, reporting: ActivityReporting) -> Result<(), String>;
    async fn unregister(&self, endpoint: &str) -> Result<(), String>;
    async fn report(&self, endpoint: &str, active: bool) -> Result<(), String>;
}

impl Service for PresenceService {
    async fn register(&self, endpoint: &str, reporting: ActivityReporting) -> Result<(), String> {
        self.register_endpoint(endpoint, reporting)
            .await
            .map_err(|error| error.to_string())
    }
    async fn unregister(&self, endpoint: &str) -> Result<(), String> {
        self.unregister_endpoint(endpoint)
            .await
            .map_err(|error| error.to_string())
    }
    async fn report(&self, endpoint: &str, active: bool) -> Result<(), String> {
        self.report_activity(endpoint, active)
            .await
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug, Default)]
pub struct Link {
    endpoint: Option<String>,
    registered: Option<ActivityReporting>,
    /// Registrations of earlier connections, still to be withdrawn.
    stale: Vec<String>,
    down_since: Option<Instant>,
    last_report: Option<(Instant, bool)>,
    failures: u32,
    next_try: Option<Instant>,
    pub problem: Option<String>,
}

impl Link {
    /// The push connection has an endpoint id; it stays the same across reconnects.
    pub fn endpoint(&mut self, id: String) {
        if self.endpoint.as_ref() != Some(&id) {
            self.endpoint = Some(id);
            self.registered = None;
        }
    }

    /// A new connection with a new endpoint follows; the old registration must not outlive it.
    pub fn restart(&mut self) {
        let mut stale = std::mem::take(&mut self.stale);
        if self.registered.is_some() {
            stale.extend(self.endpoint.take());
        }
        *self = Link {
            stale,
            ..Link::default()
        };
    }

    pub fn connected(&mut self) {
        // The service may have dropped the device while the connection was down.
        if self.down_since.take().is_some() {
            self.registered = None;
        }
        self.next_try = None;
        self.failures = 0;
    }

    pub fn disconnected(&mut self, now: Instant) {
        self.down_since.get_or_insert(now);
    }

    pub fn registered(&self) -> bool {
        self.registered.is_some()
    }

    /// Register again, because the service does not show what it was told.
    pub fn repair(&mut self) {
        self.registered = None;
        self.last_report = None;
        self.next_try = None;
    }

    /// Makes the service match `desired`. Cheap when nothing changed.
    pub async fn reconcile(&mut self, service: &impl Service, desired: Desired, now: Instant) {
        if !desired.registered {
            return self.withdraw(service).await;
        }
        let Some(endpoint) = self.endpoint.clone() else {
            return;
        };
        self.drop_stale(service).await;
        if self.next_try.is_some_and(|next| now < next) {
            return;
        }
        let down = self.down_since.map(|since| now.duration_since(since));
        let reporting = if down.is_some_and(|down| down >= DIRECT_AFTER) {
            ActivityReporting::Direct
        } else {
            ActivityReporting::Transport
        };
        if self.registered != Some(reporting) {
            // Down only briefly: the push connection comes back by itself.
            if down.is_some() && reporting == ActivityReporting::Transport {
                return;
            }
            if let Err(error) = service.register(&endpoint, reporting).await {
                return self.failed(now, error);
            }
            self.registered = Some(reporting);
            self.last_report = None;
            self.succeeded();
        }
        if reporting == ActivityReporting::Direct {
            let due = self.last_report.is_none_or(|(at, active)| {
                active != desired.active || now.duration_since(at) >= DIRECT_EVERY
            });
            if due {
                if let Err(error) = service.report(&endpoint, desired.active).await {
                    // The endpoint may have expired on the server.
                    self.registered = None;
                    return self.failed(now, error);
                }
                self.last_report = Some((now, desired.active));
                self.succeeded();
            }
        }
    }

    /// Takes this device out of the user's presence, so they show as their other devices make them.
    pub async fn withdraw(&mut self, service: &impl Service) {
        self.drop_stale(service).await;
        if self.registered.take().is_some()
            && let Some(endpoint) = &self.endpoint
            && let Err(error) = service.unregister(endpoint).await
        {
            tracing::warn!(%error, "could not withdraw this device");
        }
        self.last_report = None;
    }

    /// One try each: a registration that cannot be withdrawn expires on the server.
    async fn drop_stale(&mut self, service: &impl Service) {
        while let Some(endpoint) = self.stale.pop() {
            if let Err(error) = service.unregister(&endpoint).await {
                tracing::debug!(%error, "could not withdraw an old registration");
            }
        }
    }

    fn succeeded(&mut self) {
        self.failures = 0;
        self.next_try = None;
        self.problem = None;
    }

    fn failed(&mut self, now: Instant, error: String) {
        self.failures += 1;
        let wait = (RETRY_FIRST * 2u32.pow((self.failures - 1).min(8))).min(RETRY_MAX);
        self.next_try = Some(now + wait);
        if self.problem.as_ref() != Some(&error) {
            tracing::warn!(%error, "presence");
        }
        self.problem = Some(error);
    }
}

/// Whether Teams shows someone as away although this computer reports them active and they did
/// not choose that status themselves.
pub fn contradicts(status: &PresenceStatus, active: bool) -> bool {
    active
        && status.forced.is_none()
        && matches!(
            status.availability.as_str(),
            "Away" | "Offline" | "PresenceUnknown"
        )
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<String>>,
        failing: Cell<bool>,
    }

    impl Fake {
        fn note(&self, call: String) -> Result<(), String> {
            self.calls.borrow_mut().push(call);
            if self.failing.get() {
                Err("HTTP 503".into())
            } else {
                Ok(())
            }
        }
        fn take(&self) -> Vec<String> {
            self.calls.take()
        }
    }

    impl Service for Fake {
        async fn register(
            &self,
            endpoint: &str,
            reporting: ActivityReporting,
        ) -> Result<(), String> {
            self.note(format!("register {endpoint} {reporting:?}"))
        }
        async fn unregister(&self, endpoint: &str) -> Result<(), String> {
            self.note(format!("unregister {endpoint}"))
        }
        async fn report(&self, endpoint: &str, active: bool) -> Result<(), String> {
            self.note(format!("report {endpoint} {active}"))
        }
    }

    const ACTIVE: Desired = Desired {
        registered: true,
        active: true,
    };
    const IDLE: Desired = Desired {
        registered: true,
        active: false,
    };
    const ASLEEP: Desired = Desired {
        registered: false,
        active: false,
    };

    fn link() -> Link {
        let mut link = Link::default();
        link.endpoint("e1".into());
        link.connected();
        link
    }

    #[tokio::test]
    async fn registers_once_while_push_is_up() {
        let (fake, mut link, now) = (Fake::default(), link(), Instant::now());
        for desired in [ACTIVE, IDLE, ACTIVE] {
            link.reconcile(&fake, desired, now).await;
        }
        assert_eq!(fake.take(), ["register e1 Transport"]);
    }

    #[tokio::test]
    async fn sleep_withdraws_and_waking_registers_again() {
        let (fake, mut link, now) = (Fake::default(), link(), Instant::now());
        link.reconcile(&fake, ACTIVE, now).await;
        link.reconcile(&fake, ASLEEP, now).await;
        link.reconcile(&fake, ASLEEP, now).await;
        link.reconcile(&fake, ACTIVE, now).await;
        assert_eq!(
            fake.take(),
            [
                "register e1 Transport",
                "unregister e1",
                "register e1 Transport"
            ]
        );
    }

    #[tokio::test]
    async fn a_long_push_outage_reports_directly() {
        let (fake, mut link, start) = (Fake::default(), link(), Instant::now());
        link.reconcile(&fake, ACTIVE, start).await;
        link.disconnected(start);
        link.reconcile(&fake, ACTIVE, start + Duration::from_secs(30))
            .await;
        assert_eq!(fake.take(), ["register e1 Transport"]);
        link.reconcile(&fake, ACTIVE, start + Duration::from_secs(61))
            .await;
        link.reconcile(&fake, ACTIVE, start + Duration::from_secs(90))
            .await;
        assert_eq!(fake.take(), ["register e1 Direct", "report e1 true"]);
        link.reconcile(&fake, IDLE, start + Duration::from_secs(91))
            .await;
        link.reconcile(&fake, IDLE, start + Duration::from_secs(212))
            .await;
        assert_eq!(fake.take(), ["report e1 false", "report e1 false"]);
        link.connected();
        link.reconcile(&fake, IDLE, start + Duration::from_secs(213))
            .await;
        assert_eq!(fake.take(), ["register e1 Transport"]);
    }

    #[tokio::test]
    async fn a_new_connection_withdraws_the_old_registration() {
        let (fake, mut link, now) = (Fake::default(), link(), Instant::now());
        link.reconcile(&fake, ACTIVE, now).await;
        link.restart();
        link.restart();
        link.reconcile(&fake, ACTIVE, now).await;
        assert_eq!(fake.take(), ["register e1 Transport"]);
        link.endpoint("e2".into());
        link.reconcile(&fake, ACTIVE, now).await;
        link.reconcile(&fake, ACTIVE, now).await;
        assert_eq!(fake.take(), ["unregister e1", "register e2 Transport"]);
    }

    #[tokio::test]
    async fn a_short_outage_registers_again() {
        let (fake, mut link, now) = (Fake::default(), link(), Instant::now());
        link.reconcile(&fake, ACTIVE, now).await;
        link.disconnected(now);
        link.endpoint("e1".into());
        link.connected();
        link.reconcile(&fake, ACTIVE, now).await;
        assert_eq!(
            fake.take(),
            ["register e1 Transport", "register e1 Transport"]
        );
    }

    #[tokio::test]
    async fn failures_back_off_and_recover() {
        let (fake, mut link, start) = (Fake::default(), link(), Instant::now());
        fake.failing.set(true);
        link.reconcile(&fake, ACTIVE, start).await;
        link.reconcile(&fake, ACTIVE, start + Duration::from_secs(5))
            .await;
        assert_eq!(fake.take().len(), 1);
        assert!(link.problem.is_some() && !link.registered());
        fake.failing.set(false);
        link.reconcile(&fake, ACTIVE, start + Duration::from_secs(11))
            .await;
        assert!(link.problem.is_none() && link.registered());
    }

    #[tokio::test]
    async fn nothing_is_sent_without_an_endpoint() {
        let fake = Fake::default();
        Link::default()
            .reconcile(&fake, ACTIVE, Instant::now())
            .await;
        assert!(fake.take().is_empty());
    }

    #[test]
    fn a_chosen_status_is_not_a_contradiction() {
        use chatsvc::{ForcedAvailability, ForcedKind};
        let mut status = PresenceStatus {
            availability: "Away".into(),
            ..Default::default()
        };
        assert!(contradicts(&status, true));
        assert!(!contradicts(&status, false));
        status.forced = Some(ForcedAvailability {
            kind: ForcedKind::Away,
            expires_at: None,
        });
        assert!(!contradicts(&status, true));
        status.forced = None;
        status.availability = "Busy".into();
        assert!(!contradicts(&status, true));
    }
}
