//! Pure notification-center state for Rouch.
//!
//! Notifications are timestamped with an integer millisecond clock supplied
//! by the compositor.  That keeps expiration and queue tests deterministic
//! and leaves all wall-clock, persistence and drawing concerns to callers.
//! The center queues notifications while Do Not Disturb is active, so turning
//! the preference off does not silently lose events.

use std::{collections::VecDeque, time::Duration};

/// Stable identifier assigned by [`NotificationCenter`].
pub type NotificationId = u64;

/// Severity/visual level of a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NotificationLevel {
    Info,
    Success,
    Warning,
    Error,
    Critical,
}

impl NotificationLevel {
    /// Critical notifications can optionally bypass Do Not Disturb.
    pub fn is_critical(self) -> bool {
        self == Self::Critical
    }
}

/// A button/action exposed by a notification toast or detail view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationAction {
    /// Stable action identifier supplied by the source app.
    pub id: String,
    /// User-facing label.
    pub label: String,
    /// Whether activating this action dismisses its notification.
    pub dismisses: bool,
}

impl NotificationAction {
    /// Create a keep-open action.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            dismisses: false,
        }
    }

    /// Mark this action as one that closes its notification after activation.
    pub fn dismisses_notification(mut self, dismisses: bool) -> Self {
        self.dismisses = dismisses;
        self
    }

    /// Builder shorthand for a dismissing action.
    pub fn dismissing(self) -> Self {
        self.dismisses_notification(true)
    }
}

/// One notification in the pending queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// Assigned by the center; drafts use zero until enqueued.
    pub id: NotificationId,
    /// App/source identifier used for per-app preferences and default groups.
    pub source: String,
    /// Short title shown in the notification stack.
    pub title: String,
    /// Body text.
    pub body: String,
    /// Severity.
    pub level: NotificationLevel,
    /// Timestamp from the caller's monotonic/session clock, in milliseconds.
    pub created_at_ms: u64,
    /// Absolute expiration timestamp. `None` means persistent until dismissed.
    pub expires_at_ms: Option<u64>,
    /// Explicit group key; when absent, `source` is used.
    pub group_key: Option<String>,
    /// Actions the renderer can present.
    pub actions: Vec<NotificationAction>,
    /// Read state used by badges and the notification center.
    pub read: bool,
}

impl Notification {
    /// Construct a draft. The center assigns a unique ID on enqueue.
    pub fn new(
        source: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        level: NotificationLevel,
        created_at_ms: u64,
    ) -> Self {
        Self {
            id: 0,
            source: source.into(),
            title: title.into(),
            body: body.into(),
            level,
            created_at_ms,
            expires_at_ms: None,
            group_key: None,
            actions: Vec::new(),
            read: false,
        }
    }

    /// Supply a caller-chosen ID. IDs that collide in a center are replaced
    /// with a fresh center-generated ID.
    pub fn with_id(mut self, id: NotificationId) -> Self {
        self.id = id;
        self
    }

    /// Expire after `duration` from the notification creation timestamp.
    pub fn expires_after(mut self, duration: Duration) -> Self {
        let millis = duration.as_millis().min(u64::MAX as u128) as u64;
        self.expires_at_ms = Some(self.created_at_ms.saturating_add(millis));
        self
    }

    /// Set an absolute expiration timestamp.
    pub fn expires_at(mut self, timestamp_ms: u64) -> Self {
        self.expires_at_ms = Some(timestamp_ms);
        self
    }

    /// Make the notification persistent.
    pub fn persistent(mut self) -> Self {
        self.expires_at_ms = None;
        self
    }

    /// Place the notification in an explicit group.
    pub fn grouped_as(mut self, group_key: impl Into<String>) -> Self {
        self.group_key = Some(group_key.into());
        self
    }

    /// Add a user action.
    pub fn with_action(mut self, action: NotificationAction) -> Self {
        self.actions.push(action);
        self
    }

    /// Whether this notification has expired at the supplied timestamp.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at_ms.is_some_and(|expires_at| now_ms >= expires_at)
    }

    /// The effective group key used by the center.
    pub fn effective_group_key(&self) -> &str {
        self.group_key.as_deref().unwrap_or(&self.source)
    }
}

/// Preferences controlling delivery and visibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPreferences {
    /// Global notification switch.
    pub enabled: bool,
    /// Queue notifications without showing them while enabled.
    pub do_not_disturb: bool,
    /// Whether critical notifications may still be shown during DND.
    pub allow_critical_during_dnd: bool,
    /// Per-source switches. Missing sources are enabled by default.
    app_enabled: std::collections::BTreeMap<String, bool>,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            do_not_disturb: false,
            allow_critical_during_dnd: false,
            app_enabled: std::collections::BTreeMap::new(),
        }
    }
}

impl NotificationPreferences {
    /// Enable or disable all notification delivery. Existing queue entries are
    /// retained and become visible again if notifications are re-enabled.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Toggle Do Not Disturb. DND defers rather than drops new notifications.
    pub fn set_do_not_disturb(&mut self, enabled: bool) {
        self.do_not_disturb = enabled;
    }

    /// Alias matching the common UI abbreviation.
    pub fn set_dnd(&mut self, enabled: bool) {
        self.set_do_not_disturb(enabled);
    }

    /// Set whether critical notifications bypass DND.
    pub fn set_critical_allowed_during_dnd(&mut self, allowed: bool) {
        self.allow_critical_during_dnd = allowed;
    }

    /// Enable or disable one source. Enabling removes an unnecessary override.
    pub fn set_app_enabled(&mut self, source: impl Into<String>, enabled: bool) {
        let source = source.into();
        if enabled {
            self.app_enabled.remove(&source);
        } else {
            self.app_enabled.insert(source, false);
        }
    }

    /// Whether a source is enabled; unknown sources default to enabled.
    pub fn app_enabled(&self, source: &str) -> bool {
        self.app_enabled.get(source).copied().unwrap_or(true)
    }

    /// Remove all per-source overrides.
    pub fn reset_app_overrides(&mut self) {
        self.app_enabled.clear();
    }

    /// Explain why a notification is currently hidden, if it is hidden.
    pub fn suppression_reason(&self, notification: &Notification) -> Option<SuppressionReason> {
        if !self.enabled {
            return Some(SuppressionReason::NotificationsDisabled);
        }
        if !self.app_enabled(&notification.source) {
            return Some(SuppressionReason::SourceDisabled);
        }
        if self.do_not_disturb && !(notification.level.is_critical() && self.allow_critical_during_dnd) {
            return Some(SuppressionReason::DoNotDisturb);
        }
        None
    }
}

/// Reason a newly submitted notification was suppressed or deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressionReason {
    NotificationsDisabled,
    SourceDisabled,
    DoNotDisturb,
}

/// Result of adding one notification to the center.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueResult {
    /// The notification is in the queue and currently visible.
    Delivered(NotificationId),
    /// The notification is queued but hidden by DND.
    Deferred(NotificationId),
    /// The notification was not queued because delivery is disabled.
    Suppressed {
        id: NotificationId,
        reason: SuppressionReason,
    },
}

impl EnqueueResult {
    /// The ID assigned to the submission, regardless of delivery outcome.
    pub fn id(&self) -> NotificationId {
        match self {
            Self::Delivered(id) | Self::Deferred(id) => *id,
            Self::Suppressed { id, .. } => *id,
        }
    }

    /// Whether the notification was retained by the queue.
    pub fn queued(&self) -> bool {
        !matches!(self, Self::Suppressed { .. })
    }
}

/// A group of visible notifications from one source/key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationGroup {
    /// Effective group key.
    pub key: String,
    /// Source application identifier.
    pub source: String,
    /// Notifications in queue order, oldest first.
    pub notifications: Vec<Notification>,
}

impl NotificationGroup {
    /// Number of notifications in this group.
    pub fn count(&self) -> usize {
        self.notifications.len()
    }

    /// Number of unread notifications in this group.
    pub fn unread_count(&self) -> usize {
        self.notifications
            .iter()
            .filter(|notification| !notification.read)
            .count()
    }

    /// The newest notification in this group.
    pub fn latest(&self) -> Option<&Notification> {
        self.notifications.last()
    }
}

/// Result of activating a notification action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionResult {
    Invoked {
        notification_id: NotificationId,
        action: NotificationAction,
    },
    NotificationNotFound,
    ActionNotFound,
}

/// Pure notification queue and preference state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationCenter {
    queue: VecDeque<Notification>,
    preferences: NotificationPreferences,
    next_id: NotificationId,
}

impl Default for NotificationCenter {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationCenter {
    /// Create an enabled center with an empty queue.
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            preferences: NotificationPreferences::default(),
            next_id: 1,
        }
    }

    /// Create a center with explicit preferences.
    pub fn with_preferences(preferences: NotificationPreferences) -> Self {
        Self {
            preferences,
            ..Self::new()
        }
    }

    /// Current delivery preferences.
    pub fn preferences(&self) -> &NotificationPreferences {
        &self.preferences
    }

    /// Mutably access preferences for a settings UI.
    pub fn preferences_mut(&mut self) -> &mut NotificationPreferences {
        &mut self.preferences
    }

    /// Replace preferences without changing queued notifications.
    pub fn set_preferences(&mut self, preferences: NotificationPreferences) {
        self.preferences = preferences;
    }

    /// Number of retained notifications, including DND-deferred entries.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether no notifications are retained.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Iterate over retained notifications in queue order.
    pub fn iter(&self) -> impl Iterator<Item = &Notification> {
        self.queue.iter()
    }

    /// Add a notification, assigning a stable ID.
    pub fn enqueue(&mut self, mut notification: Notification) -> EnqueueResult {
        notification.id = self.assign_id(notification.id);
        let id = notification.id;
        let reason = self.preferences.suppression_reason(&notification);

        match reason {
            Some(SuppressionReason::NotificationsDisabled | SuppressionReason::SourceDisabled) => {
                EnqueueResult::Suppressed {
                    id,
                    reason: reason.expect("matched"),
                }
            }
            Some(SuppressionReason::DoNotDisturb) => {
                self.queue.push_back(notification);
                EnqueueResult::Deferred(id)
            }
            None => {
                self.queue.push_back(notification);
                EnqueueResult::Delivered(id)
            }
        }
    }

    /// Alias with the terminology used by notification protocols.
    pub fn push(&mut self, notification: Notification) -> EnqueueResult {
        self.enqueue(notification)
    }

    /// Notifications that are not expired and currently allowed by the
    /// preferences. DND-deferred entries remain in the queue but are omitted.
    pub fn visible_at(&self, now_ms: u64) -> Vec<&Notification> {
        self.queue
            .iter()
            .filter(|notification| {
                !notification.is_expired(now_ms)
                    && self.preferences.suppression_reason(notification).is_none()
            })
            .collect()
    }

    /// Group visible notifications while preserving first-group/queue order.
    pub fn grouped_at(&self, now_ms: u64) -> Vec<NotificationGroup> {
        let mut groups: Vec<NotificationGroup> = Vec::new();
        for notification in self.visible_at(now_ms) {
            let key = notification.effective_group_key();
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.key == key && group.source == notification.source)
            {
                group.notifications.push(notification.clone());
            } else {
                groups.push(NotificationGroup {
                    key: key.to_owned(),
                    source: notification.source.clone(),
                    notifications: vec![notification.clone()],
                });
            }
        }
        groups
    }

    /// Remove all entries whose expiration timestamp has passed.
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let before = self.queue.len();
        self.queue.retain(|notification| !notification.is_expired(now_ms));
        before - self.queue.len()
    }

    /// Mark one notification read/unread.
    pub fn mark_read(&mut self, id: NotificationId, read: bool) -> bool {
        let Some(notification) = self.queue.iter_mut().find(|notification| notification.id == id) else {
            return false;
        };
        notification.read = read;
        true
    }

    /// Mark every notification in a group read/unread.
    pub fn mark_group_read(&mut self, group_key: &str, read: bool) -> usize {
        let mut changed = 0;
        for notification in &mut self.queue {
            if notification.effective_group_key() == group_key {
                notification.read = read;
                changed += 1;
            }
        }
        changed
    }

    /// Dismiss one notification by ID.
    pub fn dismiss(&mut self, id: NotificationId) -> Option<Notification> {
        let index = self.queue.iter().position(|notification| notification.id == id)?;
        self.queue.remove(index)
    }

    /// Dismiss every notification with an effective group key.
    pub fn dismiss_group(&mut self, group_key: &str) -> usize {
        let before = self.queue.len();
        self.queue
            .retain(|notification| notification.effective_group_key() != group_key);
        before - self.queue.len()
    }

    /// Remove the oldest currently visible notification, after expiring stale
    /// entries. Hidden DND entries are left queued.
    pub fn pop_next(&mut self, now_ms: u64) -> Option<Notification> {
        self.expire(now_ms);
        let index = self
            .queue
            .iter()
            .position(|notification| self.preferences.suppression_reason(notification).is_none())?;
        self.queue.remove(index)
    }

    /// Activate an action. Dismissing actions remove the notification; other
    /// actions mark it read and leave it available for follow-up actions.
    pub fn activate_action(&mut self, id: NotificationId, action_id: &str) -> ActionResult {
        let Some(index) = self.queue.iter().position(|notification| notification.id == id) else {
            return ActionResult::NotificationNotFound;
        };
        let Some(action) = self.queue[index]
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .cloned()
        else {
            return ActionResult::ActionNotFound;
        };

        if action.dismisses {
            let _ = self.queue.remove(index);
        } else if let Some(notification) = self.queue.get_mut(index) {
            notification.read = true;
        }
        ActionResult::Invoked {
            notification_id: id,
            action,
        }
    }

    /// Dismiss all retained notifications.
    pub fn clear(&mut self) {
        self.queue.clear();
    }

    /// Count unread notifications that are currently visible.
    pub fn unread_visible_count(&self, now_ms: u64) -> usize {
        self.visible_at(now_ms)
            .into_iter()
            .filter(|notification| !notification.read)
            .count()
    }

    fn assign_id(&mut self, requested: NotificationId) -> NotificationId {
        let duplicate = requested != 0 && self.queue.iter().any(|notification| notification.id == requested);
        if requested != 0 && !duplicate {
            self.next_id = self.next_id.max(requested.saturating_add(1));
            return requested;
        }

        let id = self.next_id.max(1);
        self.next_id = id.saturating_add(1).max(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(source: &str, title: &str, at: u64) -> Notification {
        Notification::new(source, title, "body", NotificationLevel::Info, at)
    }

    #[test]
    fn enqueue_assigns_ids_and_dnd_defers_without_dropping() {
        let mut center = NotificationCenter::new();
        let first = center.enqueue(info("mail", "One", 10));
        let first_id = first.id();
        assert!(matches!(first, EnqueueResult::Delivered(_)));
        assert!(first_id > 0);

        center.preferences_mut().set_do_not_disturb(true);
        let deferred = center.enqueue(info("mail", "Two", 11));
        assert!(matches!(deferred, EnqueueResult::Deferred(_)));
        assert_eq!(center.len(), 2);
        assert_eq!(center.visible_at(11).len(), 0);

        center.preferences_mut().set_do_not_disturb(false);
        assert_eq!(center.visible_at(11).len(), 2);
    }

    #[test]
    fn global_and_per_source_disable_suppress_new_events() {
        let mut center = NotificationCenter::new();
        center.preferences_mut().set_app_enabled("chat", false);
        let source_disabled = center.enqueue(info("chat", "Nope", 0));
        assert_eq!(
            source_disabled,
            EnqueueResult::Suppressed {
                id: source_disabled.id(),
                reason: SuppressionReason::SourceDisabled,
            }
        );
        assert!(center.is_empty());

        center.preferences_mut().set_enabled(false);
        let global_disabled = center.enqueue(info("mail", "Nope", 1));
        assert!(matches!(
            global_disabled,
            EnqueueResult::Suppressed {
                reason: SuppressionReason::NotificationsDisabled,
                ..
            }
        ));
        assert!(center.is_empty());
    }

    #[test]
    fn grouping_uses_explicit_key_and_preserves_queue_order() {
        let mut center = NotificationCenter::new();
        center.enqueue(info("mail", "First", 0).grouped_as("inbox"));
        center.enqueue(info("mail", "Second", 1).grouped_as("inbox"));
        center.enqueue(info("calendar", "Meeting", 2));

        let groups = center.grouped_at(2);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].key, "inbox");
        assert_eq!(groups[0].count(), 2);
        assert_eq!(groups[0].latest().unwrap().title, "Second");
        assert_eq!(groups[1].source, "calendar");
    }

    #[test]
    fn expiration_is_deterministic_and_boundary_inclusive() {
        let mut center = NotificationCenter::new();
        center.enqueue(info("timer", "Short", 100).expires_after(Duration::from_millis(50)));
        center.enqueue(info("timer", "Persistent", 100));
        assert_eq!(center.visible_at(149).len(), 2);
        assert_eq!(center.visible_at(150).len(), 1);
        assert_eq!(center.expire(150), 1);
        assert_eq!(center.len(), 1);
    }

    #[test]
    fn actions_can_keep_open_or_dismiss() {
        let mut center = NotificationCenter::new();
        let keep_id = center
            .enqueue(info("sync", "Done", 0).with_action(NotificationAction::new("view", "View")))
            .id();
        let remove_id = center
            .enqueue(
                info("sync", "Error", 1).with_action(NotificationAction::new("retry", "Retry").dismissing()),
            )
            .id();

        assert!(matches!(
            center.activate_action(keep_id, "view"),
            ActionResult::Invoked { .. }
        ));
        assert!(center.iter().find(|item| item.id == keep_id).unwrap().read);
        assert!(matches!(
            center.activate_action(remove_id, "retry"),
            ActionResult::Invoked { .. }
        ));
        assert!(center.iter().all(|item| item.id != remove_id));
        assert_eq!(
            center.activate_action(keep_id, "missing"),
            ActionResult::ActionNotFound
        );
    }

    #[test]
    fn critical_notifications_can_optionally_bypass_dnd() {
        let mut center = NotificationCenter::new();
        center.preferences_mut().set_do_not_disturb(true);
        let deferred = center.enqueue(Notification::new(
            "system",
            "Critical",
            "Battery failure",
            NotificationLevel::Critical,
            0,
        ));
        assert!(matches!(deferred, EnqueueResult::Deferred(_)));
        center.preferences_mut().set_critical_allowed_during_dnd(true);
        let delivered = center.enqueue(Notification::new(
            "system",
            "Critical 2",
            "Battery failure",
            NotificationLevel::Critical,
            1,
        ));
        assert!(matches!(delivered, EnqueueResult::Delivered(_)));
        // Enabling the exception changes visibility for the already queued
        // critical notification too; preference changes never drop retained
        // events.
        let visible = center.visible_at(1);
        assert_eq!(visible.len(), 2);
        assert!(
            visible
                .iter()
                .any(|notification| notification.title == "Critical")
        );
        assert!(
            visible
                .iter()
                .any(|notification| notification.title == "Critical 2")
        );
    }

    #[test]
    fn pop_next_skips_hidden_dnd_entries_until_preferences_change() {
        let mut center = NotificationCenter::new();
        center.preferences_mut().set_do_not_disturb(true);
        center.enqueue(info("mail", "Deferred", 0));
        assert!(center.pop_next(0).is_none());
        center.preferences_mut().set_do_not_disturb(false);
        assert_eq!(center.pop_next(0).unwrap().title, "Deferred");
    }
}
