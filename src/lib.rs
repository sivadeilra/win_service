#![allow(unused)]

use core::ptr::null_mut;
use core::u32;
use log::{error, info, warn};
use std::sync::mpsc;
use std::sync::{Condvar, Mutex};
use std::time::Duration;
use widestring::{U16CStr, U16CString};
use winapi::shared::guiddef::GUID;
use winapi::shared::winerror;
use winapi::um::winnt::LPWSTR;
use winapi::um::winsvc;
use winapi::um::winuser::{PBT_POWERSETTINGCHANGE, POWERBROADCAST_SETTING};

pub extern crate widestring;

// this is missing from winapi
const SERVICE_USER_OWN_PROCESS: u32 = 0x50;

type WCHAR = u16;

pub struct Error {}

pub trait ServiceHandler {
    fn service_name(&self) -> &str;
    // fn create(updater: &mut StatusUpdater) -> Self;
    fn start(&mut self, updater: &mut StatusUpdater) -> Result<(), ServiceError> {
        Ok(())
    }
    fn resume(&mut self, updater: &mut StatusUpdater) {}
    fn pause(&mut self, updater: &mut StatusUpdater) {}
    fn stop(&mut self, updater: &mut StatusUpdater) {}
    fn shutdown(&mut self, updater: &mut StatusUpdater) {}
    fn preshutdown(&mut self, updater: &mut StatusUpdater) {}

    fn param_change(&mut self) {}
    fn power_setting(&mut self, power_setting: &GUID, data: &[u8]) {}
}

#[derive(Debug)]
pub enum ServiceError {
    Failed,
}

struct ServiceStatusHandle {
    handle: winsvc::SERVICE_STATUS_HANDLE,
}

pub struct StatusUpdater {
    service_status_handle: winsvc::SERVICE_STATUS_HANDLE,
    checkpoint: u32,
    current_state: u32,
    service_type: u32,
    controls_accepted: u32,
}

impl StatusUpdater {
    pub fn checkpoint_with_hint(&mut self, wait_hint: Duration) {
        self.send_update(wait_hint);
        self.checkpoint += 1;
    }

    fn send_update(&mut self, wait_hint: Duration) {
        let mut status = winsvc::SERVICE_STATUS {
            dwCheckPoint: self.checkpoint,
            dwControlsAccepted: self.controls_accepted,
            dwCurrentState: self.current_state,
            dwWaitHint: wait_hint.as_millis().max(u32::MAX.into()) as u32,
            dwServiceSpecificExitCode: 0,
            dwServiceType: self.service_type,
            dwWin32ExitCode: 0,
        };

        unsafe {
            winsvc::SetServiceStatus(
                self.service_status_handle,
                &mut status as *mut winsvc::SERVICE_STATUS,
            );
        }
    }

    pub fn checkpoint(&mut self) {
        self.checkpoint_with_hint(Duration::from_secs(0));
    }

    fn set_state(&mut self, state: u32) {
        self.current_state = state;
        self.checkpoint = 0;
    }

    fn set_accept_bits(&mut self, mask: u32, value: bool) {
        if value {
            self.controls_accepted |= mask;
        } else {
            self.controls_accepted &= !mask;
        }
    }

    pub fn accepts_stop(&mut self, value: bool) {
        self.set_accept_bits(winsvc::SERVICE_ACCEPT_STOP, value);
    }

    pub fn accepts_shutdown(&mut self, value: bool) {
        self.set_accept_bits(winsvc::SERVICE_ACCEPT_SHUTDOWN, value);
    }

    pub fn accepts_param_change(&mut self, value: bool) {
        self.set_accept_bits(winsvc::SERVICE_ACCEPT_PARAMCHANGE, value);
    }

    pub fn accepts_pause(&mut self, value: bool) {
        self.set_accept_bits(winsvc::SERVICE_ACCEPT_PAUSE_CONTINUE, value);
    }

    pub fn accepts_power_event(&mut self, value: bool) {
        self.set_accept_bits(winsvc::SERVICE_ACCEPT_POWEREVENT, value);
    }
}

pub enum ServiceControl {
    Pause,
    Resume,
    Stop,
}

pub struct ServiceArgs {
    pub status_updater: StatusUpdater,
    pub args: Vec<String>,
}

pub struct ServiceEntry<'a> {
    pub name: &'a str,
    pub creator: fn() -> Box<dyn ServiceHandler>,
}

// This is the context that is passed to RegisterServiceCtrlDispatcherEx.
struct ServiceControlHandlerContext<'a> {
    state: Mutex<ServiceState<'a>>,
    condvar: Condvar,
}

struct ServiceState<'a> {
    status_updater: StatusUpdater,
    handler: &'a mut dyn ServiceHandler,
}

// https://docs.microsoft.com/en-us/windows/win32/api/winsvc/nc-winsvc-lphandler_function_ex
unsafe extern "system" fn service_control_handler(
    control: u32,
    event_type: u32,
    event_data: *mut winapi::ctypes::c_void,
    context: *mut winapi::ctypes::c_void,
) -> u32 {
    let context: &ServiceControlHandlerContext = &*(context as *mut ServiceControlHandlerContext);

    let mut state_guard = context.state.lock().unwrap();
    let state = &mut *state_guard;
    let status_updater = &mut state.status_updater;

    // After receiving a SERVICE_CONTROL_STOP request, the SCM is never supposed to
    // send another service control request.
    assert_ne!(
        status_updater.current_state,
        winsvc::SERVICE_STOPPED,
        "Should never receive a service control request in SERVICE_STOPPED state."
    );

    match control {
        winsvc::SERVICE_CONTROL_STOP => {
            // SERVICE_CONTROL_STOP is special; after the SCM sends this control request,
            // it will never invoke the service control handler again.
            info!("Received SERVICE_CONTROL_STOP");
            status_updater.set_state(winsvc::SERVICE_STOP_PENDING);
            info!("Calling stop() function");
            state.handler.stop(status_updater);
            info!("Service state is SERVICE_STOPPED");
            status_updater.set_state(winsvc::SERVICE_STOPPED);
            status_updater.checkpoint();
            drop(state);
            context.condvar.notify_all();
        }
        winsvc::SERVICE_CONTROL_INTERROGATE => {
            info!("Received SERVICE_CONTROL_INTERROGATE");
            status_updater.send_update(Duration::from_micros(0));
        }
        winsvc::SERVICE_CONTROL_PAUSE => {
            state.handler.pause(status_updater);
        }
        winsvc::SERVICE_CONTROL_CONTINUE => {
            info!("Received SERVICE_CONTROL_CONTINUE");
            if status_updater.current_state != winsvc::SERVICE_RUNNING {
                error!("Received SERVICE_CONTROL_CONTINUE, but current state is not SERVICE_RUNNING (is {})",
                status_updater.current_state);
                return winerror::ERROR_INVALID_STATE;
            }
            status_updater.set_state(winsvc::SERVICE_CONTINUE_PENDING);
            status_updater.checkpoint();
            info!("Calling resume() function");
            state.handler.resume(status_updater);
            info!("Service state is SERVICE_RUNNING");
            status_updater.set_state(winsvc::SERVICE_RUNNING);
            status_updater.checkpoint();
            return winerror::NO_ERROR;
        }
        winsvc::SERVICE_CONTROL_PARAMCHANGE => {
            info!("Received SERVICE_CONTROL_PARAMCHANGE");
            state.handler.param_change();
            return winerror::NO_ERROR;
        }

        winsvc::SERVICE_CONTROL_POWEREVENT => {
            info!("Received SERVICE_CONTROL_POWEREVENT");
            const PBT_POWERSETTINGCHANGE_U32: u32 = PBT_POWERSETTINGCHANGE as u32;
            match event_type {
                PBT_POWERSETTINGCHANGE_U32 => {
                    let power_setting_change = event_data as *const POWERBROADCAST_SETTING;
                    state.handler.power_setting(
                        &(*power_setting_change).PowerSetting,
                        core::slice::from_raw_parts::<u8>(
                            &(*power_setting_change).Data as *const u8,
                            (*power_setting_change).DataLength as usize,
                        ),
                    );
                }
                unrecognized_event_type => {
                    info!("Received SERVICE_CONTROL_POWEREVENT, but the event type {} is not recognized.", unrecognized_event_type);
                }
            }
        }
        unrecognized_control => {
            info!(
                "Received unrecognized service control ({:#x})",
                unrecognized_control
            );
            return winerror::ERROR_CALL_NOT_IMPLEMENTED;
        }
    }

    winerror::NO_ERROR
}

unsafe extern "system" fn service_proc<S: ServiceHandler + Default>(
    num_service_args: u32,
    service_args: *mut LPWSTR,
) {
    let mut service_impl: S = S::default();
    let service_handler = &mut service_impl;

    let service_name = service_handler.service_name();
    info!("service_main starting for: {}", service_name);

    unsafe {
        let service_name_wstr = U16CString::from_str(service_name).unwrap();

        let service_control_handler_context = ServiceControlHandlerContext {
            state: Mutex::new(ServiceState {
                status_updater: StatusUpdater {
                    controls_accepted: winsvc::SERVICE_ACCEPT_STOP,
                    checkpoint: 0,
                    service_status_handle: null_mut(),
                    service_type: SERVICE_USER_OWN_PROCESS,
                    current_state: winsvc::SERVICE_START_PENDING,
                },
                handler: service_handler,
            }),
            condvar: Condvar::new(),
        };

        let service_status_handle = winsvc::RegisterServiceCtrlHandlerExW(
            service_name_wstr.as_ptr(),
            Some(service_control_handler),
            &service_control_handler_context as *const ServiceControlHandlerContext<'_>
                as *mut ServiceControlHandlerContext<'_> as *mut winapi::ctypes::c_void,
        );
        if service_status_handle.is_null() {
            error!("RegisterServiceCtrlHandlerExW failed");
            return;
        }

        {
            let mut state_guard = service_control_handler_context.state.lock().unwrap();
            let state = &mut *state_guard;
            state.status_updater.service_status_handle = service_status_handle;

            info!("sending status update for START_PENDING");
            let status_updater = &mut state.status_updater;
            status_updater.checkpoint();

            // <-- state is SERVICE_START_PENDING
            // Call into the service code to start it.
            match state.handler.start(status_updater) {
                Err(e) => {
                    error!("service failed to start: {:?}", e);
                    status_updater.set_state(winsvc::SERVICE_STOPPED);
                    status_updater.checkpoint();
                    return;
                }
                Ok(()) => {}
            }

            info!("Sending SERVICE_RUNNING");
            status_updater.set_state(winsvc::SERVICE_RUNNING);
            status_updater.checkpoint();
        }

        info!("service has successfully started.");
        

        // Now we just wait to receive the "stop" request.
        let mut state_guard = service_control_handler_context.state.lock().unwrap();
        loop {
            if state_guard.status_updater.current_state == winsvc::SERVICE_STOPPED {
                info!("service_main: service is stopped; exiting thread.");
                break;
            }
            state_guard = service_control_handler_context
                .condvar
                .wait(state_guard)
                .unwrap();
        }
    }
}

pub fn single_service_main<S: ServiceHandler + Default>(service_name: &str) {
    unsafe {
        let service_name_wstr = U16CString::from_str(service_name).unwrap();
        let service_table = [
            winsvc::SERVICE_TABLE_ENTRYW {
            lpServiceName: service_name_wstr.as_ptr(),
            lpServiceProc: Some(service_proc::<S>),
        },
        winsvc::SERVICE_TABLE_ENTRYW {
            lpServiceName: null_mut(),
            lpServiceProc: None,
        },
        ];
        info!("Calling StartServiceCtrlDispatcherW");
        if winsvc::StartServiceCtrlDispatcherW(&service_table[0]) != 0 {
            // succeeded
        } else {
            error!("StartServiceCtrlDispatcherW failed");
        }
    }
}

#[macro_export]
macro_rules! single_service {
    (
        $name:expr,
        $service_type:ty
    ) => {
        pub fn main() {
            $crate::single_service_main::<$service_type>($name);
        }
    };
}
