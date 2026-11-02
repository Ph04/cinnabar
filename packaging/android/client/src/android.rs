//! The NativeActivity entry that runs the client, and the setup activity's natives.

mod bootstrap;

use std::{
    path::{Path, PathBuf},
    sync::{OnceLock, atomic::{AtomicBool, Ordering}},
};

use anyhow::{Context, Result};
use bevy::android::{ANDROID_APP, android_activity::AndroidApp};
use jni::{
    JavaVM,
    objects::{JObject, JString, JValue},
};
use launcher::install_layout::{InstallLayout, configure_android};

/// Prevents a persistent activity callback failure from filling the diagnostic log every frame.
static STARTUP_PROGRESS_FAILED: AtomicBool = AtomicBool::new(false);
static STARTUP_GENERATION_FAILED: AtomicBool = AtomicBool::new(false);

/// Names shared with `packaging/android/build-apk.sh`.
#[derive(serde::Deserialize)]
struct Runtime {
    application_id: String,
    core_library: String,
    compiler_library: String,
    shaderpack_worker_library: String,
}

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        serde_json::from_str(include_str!("../../runtime.json"))
            .expect("the tracked Android runtime manifest parses")
    })
}

/// The install layout for the app's private files and the APK's native-library directory.
fn layout(files_dir: &Path, native_library_dir: &Path) -> InstallLayout {
    InstallLayout::android(
        files_dir,
        native_library_dir.join(&runtime().core_library),
        native_library_dir.join(&runtime().shaderpack_worker_library),
    )
}

#[bevy::prelude::bevy_main]
#[expect(
    clippy::print_stderr,
    reason = "startup can fail before client logging is installed, so stderr is the only sink"
)]
fn main() {
    let code = match run_client() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{} failed: {error:#}", launcher::PRODUCT_NAME);
            if let Err(display_error) = show_startup_failure(&error) {
                eprintln!("Cannot show startup failure: {display_error:#}");
            }
            1
        }
    };
    // Bevy's activity handle and winit's event loop are once per process, so a later launch
    // needs a fresh process; the activity's onDestroy also ends it.
    std::process::exit(code);
}

/// Shows the native error dialog and services activity events until Android closes the app.
fn show_startup_failure(error: &anyhow::Error) -> Result<()> {
    let app = ANDROID_APP
        .get()
        .context("Android activity is unavailable")?;
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }?;
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let display = format!("{error:#}");
    let shown = (|| -> Result<()> {
        let message = env.new_string(display)?;
        env.call_method(
            &activity,
            "startupFailed",
            "(Ljava/lang/String;)V",
            &[(&message).into()],
        )?;
        Ok(())
    })();
    if let Err(display_error) = shown {
        record_jni_failure(&mut env, &activity, "startupFailed", &display_error);
        return Err(display_error);
    }
    loop {
        let mut destroyed = false;
        let mut input_available = false;
        app.poll_events(None, |event| {
            use bevy::android::android_activity::{MainEvent, PollEvent};
            match event {
                PollEvent::Main(MainEvent::Destroy) => destroyed = true,
                PollEvent::Main(MainEvent::InputAvailable) => input_available = true,
                _ => {}
            }
        });
        if destroyed {
            return Ok(());
        }
        if input_available {
            let mut events = app.input_events_iter()?;
            while events.next(|_| bevy::android::android_activity::InputStatus::Unhandled) {}
        }
    }
}

fn run_client() -> Result<()> {
    let app = ANDROID_APP
        .get()
        .context("Android activity is unavailable")?;
    let files_dir = app
        .internal_data_path()
        .context("Android reported no private files directory")?;
    configure_android(layout(&files_dir, &native_library_dir(app)?))?;
    cinnabar::startup_feedback::observe(startup_progress, startup_generation);
    if cinnabar::lifecycle::before_run(false)? {
        cinnabar::run(cinnabar::args::ClientArgs {
            force_vsync: true,
            ..Default::default()
        })?;
    }
    Ok(())
}

/// `ApplicationInfo.nativeLibraryDir`, which changes with every APK install.
fn native_library_dir(app: &AndroidApp) -> Result<PathBuf> {
    // The activity owns both pointers for the life of this native thread.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }?;
    let mut env = vm.attach_current_thread_permanently()?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let result = (|| -> Result<PathBuf> {
        let info = env
            .call_method(
                &activity,
                "getApplicationInfo",
                "()Landroid/content/pm/ApplicationInfo;",
                &[],
            )?
            .l()?;
        let directory = JString::from(
            env.get_field(&info, "nativeLibraryDir", "Ljava/lang/String;")?
                .l()?,
        );
        let directory: String = env.get_string(&directory)?.into();
        Ok(PathBuf::from(directory))
    })();
    if let Err(error) = &result {
        record_jni_failure(&mut env, &activity, "nativeLibraryDir", error);
    }
    result
}

/// Posts work or content readiness without blocking the activity's UI thread.
fn startup_progress(message: launcher::startup::Message, generation: u64) {
    let report = (|| -> Result<()> {
        let app = ANDROID_APP
            .get()
            .context("Android activity is unavailable")?;
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }?;
        let mut env = vm.attach_current_thread()?;
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let text = env.new_string(serde_json::to_string(&message)?)?;
        let result = env.call_method(
            &activity,
            "startupProgress",
            "(Ljava/lang/String;J)V",
            &[
                (&text).into(),
                jni::objects::JValue::Long(generation as i64),
            ],
        );
        if let Err(error) = result {
            let error: anyhow::Error = error.into();
            if !STARTUP_PROGRESS_FAILED.swap(true, Ordering::AcqRel) {
                record_jni_failure(&mut env, &activity, "startupProgress", &error);
            }
            return Err(error);
        }
        Ok(())
    })();
    if let Err(error) = report {
        bevy::log::warn!("startup feedback unavailable: {error:#}");
    }
}
/// Samples the activity lifetime before validating the next destination frame.
fn startup_generation() -> u64 {
    let read = (|| -> Result<u64> {
        let app = ANDROID_APP
            .get()
            .context("Android activity is unavailable")?;
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }?;
        let mut env = vm.attach_current_thread()?;
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let result = env.call_method(&activity, "startupGeneration", "()J", &[]);
        match result {
            Ok(value) => Ok(value.j()? as u64),
            Err(error) => {
                let error: anyhow::Error = error.into();
                if !STARTUP_GENERATION_FAILED.swap(true, Ordering::AcqRel) {
                    record_jni_failure(&mut env, &activity, "startupGeneration", &error);
                }
                Err(error)
            }
        }
    })();
    read.unwrap_or_default()
}

/// Writes the native-side error and any still-pending Java throwable to the app's bounded log.
fn record_jni_failure(
    env: &mut jni::JNIEnv<'_>,
    activity: &JObject<'_>,
    stage: &str,
    error: &anyhow::Error,
) {
    let mut details = format!("{error:#}");
    if env.exception_check().unwrap_or(false) {
        let throwable = env.exception_occurred().ok().map(JObject::from);
        let _ = env.exception_clear();
        if let Some(throwable) = throwable {
            if let Ok(value) = env.call_static_method(
                "android/util/Log",
                "getStackTraceString",
                "(Ljava/lang/Throwable;)Ljava/lang/String;",
                &[JValue::Object(&throwable)],
            ).and_then(|value| value.l()) {
                let text = JString::from(value);
                if let Ok(value) = env.get_string(&text) {
                    details = value.into();
                }
            }
        }
    }
    let logged = (|| -> jni::errors::Result<()> {
        let stage = env.new_string(stage)?;
        let details = env.new_string(details)?;
        env.call_method(
            activity,
            "recordJniException",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Object(&stage), JValue::Object(&details)],
        )?;
        Ok(())
    })();
    if logged.is_err() && env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}
