#![allow(unused)]

use log::{error, info};
use std::net::{TcpListener, TcpStream};
use win_service::{ServiceError, StatusUpdater};

#[derive(Default)]
struct HelloService {
    listener: Option<TcpListener>,
}

impl win_service::ServiceHandler for HelloService {
    fn service_name(&self) -> &str {
        "hello_service"
    }

    fn start(&mut self, updater: &mut StatusUpdater) -> Result<(), ServiceError> {
        info!("hello_server is starting");

        let listener = TcpListener::bind(":8080").map_err(|e| {
            error!("Failed to create TCP listener: {:?}", e);
            ServiceError::Failed
        })?;

        info!("successfully created TCP listener");

        self.listener = Some(listener);
        Ok(())
    }

    fn stop(&mut self, updater: &mut StatusUpdater) {
        info!("hello_server is stopping");
        self.listener = None;
    }
}

// win_service::single_service!("hello_service", HelloService);

fn main() {
    env_logger::init();
    win_service::single_service_main::<HelloService>("hello_service");
}
