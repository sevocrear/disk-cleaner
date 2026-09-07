pub mod apps;
pub mod caches;
pub mod docker;
pub mod dupes;
pub mod files;
pub mod media;

pub use apps::phase_apps;
pub use caches::phase_caches;
pub use docker::phase_docker;
pub use dupes::phase_dupes;
pub use files::phase_files;
pub use media::phase_media;
