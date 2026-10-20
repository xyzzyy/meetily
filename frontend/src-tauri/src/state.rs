use crate::database::manager::DatabaseManager;
use crate::database::speaker_store::SpeakerFeatureStore;

pub struct AppState {
    pub db_manager: DatabaseManager,
    pub speaker_store: SpeakerFeatureStore,
}
