use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard, OwnedMutexGuard};

static MEETINGS: LazyLock<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PROCESSING: AsyncMutex<()> = AsyncMutex::const_new(());

/// Keep speaker processing and manual corrections from changing the same meeting.
pub fn try_meeting_edit(meeting_id: &str) -> Result<OwnedMutexGuard<()>, String> {
    let mut meetings = MEETINGS
        .lock()
        .map_err(|_| "Speaker operation lock failed")?;
    meetings.retain(|_, lock| lock.strong_count() > 0);
    let lock = meetings
        .get(meeting_id)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| {
            let lock = Arc::new(AsyncMutex::new(()));
            meetings.insert(meeting_id.to_string(), Arc::downgrade(&lock));
            lock
        });
    lock.try_lock_owned().map_err(|_| {
        "Speaker processing or another correction is in progress. Wait for it to finish."
            .to_string()
    })
}

/// The model helper and cancellation flag support one processing operation at a time.
pub fn try_processing_slot() -> Result<MutexGuard<'static, ()>, String> {
    PROCESSING.try_lock().map_err(|_| {
        "Another speaker model operation is in progress. Wait for it to finish.".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_cannot_enter_an_active_meeting() {
        let guard = try_meeting_edit("lock-test-meeting").unwrap();
        assert!(try_meeting_edit("lock-test-meeting").is_err());
        assert!(try_meeting_edit("another-lock-test-meeting").is_ok());
        drop(guard);
        assert!(try_meeting_edit("lock-test-meeting").is_ok());
    }
}
