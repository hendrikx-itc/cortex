use std::os::unix::fs::symlink;
use std::fs::{hard_link, copy};
use std::path::PathBuf;

use futures::stream::Stream;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::event::FileEvent;
use crate::{settings, settings::LocalTargetMethod};

pub fn to_stream(
    settings: &settings::DirectoryTarget,
    receiver: UnboundedReceiver<FileEvent>,
) -> impl futures::Stream<Item = FileEvent, Error = ()> {
    let target_name = settings.name.clone();
    let target_directory = settings.directory.clone();
    let method = settings.method.clone();

    receiver.map_err(|e| {
        error!("[E01006] Error receiving: {}", e);
    }).map(move |file_event: FileEvent| -> FileEvent {
        let source_path_str = file_event.path.to_str().unwrap();
        let file_name = file_event.path.file_name().unwrap();
        let target_path = target_directory.join(file_name);
        let target_path_str = target_path.to_str().unwrap();

        debug!("FileEvent for {}: '{}'", &target_name, &source_path_str);

        match method {
            LocalTargetMethod::Copy => {
                let result = copy(&file_event.path, &target_path);

                match result {
                    Ok(size) => {
                        debug!("'{}' copied {} bytes to '{}'", &source_path_str, size, &target_path_str);
                    }
                    Err(e) => {
                        error!(
                            "[E01005] Error copying '{}' to '{}': {}",
                            &source_path_str, &target_path_str, &e
                        );
                    }
                }
            },
            LocalTargetMethod::Hardlink => {
                let result = hard_link(&file_event.path, &target_path);

                match result {
                    Ok(()) => {
                        debug!("Hardlinked '{}' to '{}'", &source_path_str, &target_path_str);
                    }
                    Err(e) => {
                        error!(
                            "[E01004] Error hardlinking '{}' to '{}': {}",
                            &source_path_str, &target_path_str, &e
                        );
                    }
                }
            },
            LocalTargetMethod::Symlink => {
                let result = symlink(&file_event.path, &target_path);

                match result {
                    Ok(()) => {
                        debug!("Symlinked '{}' to '{}'", &source_path_str, &target_path_str);
                    }
                    Err(e) => {
                        error!(
                            "[E01007] Error symlinking '{}' to '{}': {}",
                            &source_path_str, &target_path_str, &e
                        );
                    }
                }
            }
        }

        FileEvent {
            source_name: target_name.clone(),
            path: PathBuf::from(target_path)
        }
    })
}
