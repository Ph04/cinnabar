//! Browser and document-picker handoff through Android intents, since Android has no desktop
//! opener or picker tools.

use jni::{
    JNIEnv,
    objects::{JClass, JObject, JObjectArray, JString},
};
use std::{path::PathBuf, sync::atomic::{AtomicBool, Ordering}};

/// Avoids repeating a failed Android memory-info JNI call every one-second sample.
static MEMORY_INFO_FAILURE_REPORTED: AtomicBool = AtomicBool::new(false);

/// `Intent.FLAG_ACTIVITY_NEW_TASK`, required to start an activity from the application context.
const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;

/// Starts a browsable `ACTION_VIEW` intent; `Ok(false)` means no app accepted the link.
pub(super) fn open_url(url: &str) -> jni::errors::Result<bool> {
    let context = ndk_context::android_context();
    // The Android runtime owns the VM and the application context for the process lifetime.
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast())? };
    let mut env = vm.attach_current_thread()?;
    // android-activity publishes the Application here, not the hosting activity.
    let application = unsafe { JObject::from_raw(context.context().cast()) };
    let result = start_view_intent(&mut env, &application, url);
    if result.is_err() && env.exception_check()? {
        env.exception_clear()?;
        return Ok(false);
    }
    result.map(|()| true)
}

/// Builds `new Intent(ACTION_VIEW, Uri.parse(url))` and starts it in a new task.
fn start_view_intent(env: &mut JNIEnv, context: &JObject, url: &str) -> jni::errors::Result<()> {
    let url = env.new_string(url)?;
    let uri = env
        .call_static_method(
            "android/net/Uri",
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[(&url).into()],
        )?
        .l()?;
    let action = env.new_string("android.intent.action.VIEW")?;
    let intent = env.new_object(
        "android/content/Intent",
        "(Ljava/lang/String;Landroid/net/Uri;)V",
        &[(&action).into(), (&uri).into()],
    )?;
    let category = env.new_string("android.intent.category.BROWSABLE")?;
    env.call_method(
        &intent,
        "addCategory",
        "(Ljava/lang/String;)Landroid/content/Intent;",
        &[(&category).into()],
    )?;
    env.call_method(
        &intent,
        "addFlags",
        "(I)Landroid/content/Intent;",
        &[FLAG_ACTIVITY_NEW_TASK.into()],
    )?;
    env.call_method(
        context,
        "startActivity",
        "(Landroid/content/Intent;)V",
        &[(&intent).into()],
    )?;
    Ok(())
}

/// Opens the Storage Access Framework picker over every document and blocks until the user
/// chooses or cancels; the caller checks filename extensions.
///
/// Returns private copies of the chosen documents, an empty list on cancellation, `None` when no
/// activity or document provider could show the picker, or an error when a document exceeds
/// `max_bytes` or could not be copied.
pub(super) fn pick_documents(
    multiple: bool,
    max_bytes: Option<u64>,
) -> Result<Option<Vec<PathBuf>>, String> {
    let context = ndk_context::android_context();
    // The Android runtime owns the VM and the application context for the process lifetime.
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let application = unsafe { JObject::from_raw(context.context().cast()) };
    // Java has no unsigned long; a negative limit means unlimited.
    let max_bytes = max_bytes.map_or(-1, |bytes| i64::try_from(bytes).unwrap_or(i64::MAX));
    let result = call_picker(&mut env, &application, multiple, max_bytes);
    result.map_err(|error| pending_exception(&mut env).unwrap_or_else(|| error.to_string()))
}

/// Calls `FilePicker.pick`, loading the class through the app's loader because worker threads
/// attached from native code only see system classes.
fn call_picker(
    env: &mut JNIEnv,
    application: &JObject,
    multiple: bool,
    max_bytes: i64,
) -> jni::errors::Result<Option<Vec<PathBuf>>> {
    let package = env
        .call_method(application, "getPackageName", "()Ljava/lang/String;", &[])?
        .l()?;
    let package: String = env.get_string(&JString::from(package))?.into();
    let loader = env
        .call_method(
            application,
            "getClassLoader",
            "()Ljava/lang/ClassLoader;",
            &[],
        )?
        .l()?;
    let name = env.new_string(format!("{package}.FilePicker"))?;
    let class = env
        .call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[(&name).into()],
        )?
        .l()?;
    let paths = env
        .call_static_method(
            JClass::from(class),
            "pick",
            "(ZJ)[Ljava/lang/String;",
            &[multiple.into(), max_bytes.into()],
        )?
        .l()?;
    if paths.is_null() {
        return Ok(None);
    }
    let paths = JObjectArray::from(paths);
    let count = env.get_array_length(&paths)?;
    let mut picked = Vec::with_capacity(count.max(0) as usize);
    for index in 0..count {
        let path = JString::from(env.get_object_array_element(&paths, index)?);
        let path: String = env.get_string(&path)?.into();
        picked.push(PathBuf::from(path));
    }
    Ok(Some(picked))
}

/// Clears a pending Java exception and returns its message, or its class when it has none, for
/// the import status line.
fn pending_exception(env: &mut JNIEnv) -> Option<String> {
    let throwable = env.exception_occurred().ok()?;
    if throwable.is_null() {
        return None;
    }
    env.exception_clear().ok()?;
    for method in ["getMessage", "toString"] {
        let message = env
            .call_method(&throwable, method, "()Ljava/lang/String;", &[])
            .and_then(|value| value.l())
            .ok()?;
        if !message.is_null() {
            return Some(env.get_string(&JString::from(message)).ok()?.into());
        }
    }
    None
}

/// Reads Android's current system memory headroom from ActivityManager.MemoryInfo.
/// Returns (total, available, low-memory threshold, low-memory flag), in bytes.
pub(crate) fn memory_info() -> Option<(u64, u64, u64, bool)> {
    let context = ndk_context::android_context();
    // Android owns the VM and application context for the process lifetime.
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    // SAFETY: ndk-context publishes a live application Context for the process lifetime.
    let application = unsafe { JObject::from_raw(context.context().cast()) };
    let result = (|| -> jni::errors::Result<Option<(u64, u64, u64, bool)>> {
        let service_name = env.new_string("activity")?;
        let manager = env
            .call_method(
                &application,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[(&service_name).into()],
            )?
            .l()?;
        if manager.is_null() {
            return Ok(None);
        }
        let info = env.new_object("android/app/ActivityManager$MemoryInfo", "()V", &[])?;
        env.call_method(
            &manager,
            "getMemoryInfo",
            "(Landroid/app/ActivityManager$MemoryInfo;)V",
            &[(&info).into()],
        )?;
        let total = env.get_field(&info, "totalMem", "J")?.j()?;
        let available = env.get_field(&info, "availMem", "J")?.j()?;
        let threshold = env.get_field(&info, "threshold", "J")?.j()?;
        let low_memory = env.get_field(&info, "lowMemory", "Z")?.z()?;
        let Ok(total) = u64::try_from(total) else {
            return Ok(None);
        };
        if total == 0 {
            return Ok(None);
        }
        let available = u64::try_from(available).unwrap_or(0);
        let threshold = u64::try_from(threshold).unwrap_or(0);
        Ok(Some((total, available, threshold, low_memory)))
    })();
    match result {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let details = pending_stack_trace(&mut env).unwrap_or_else(|| error.to_string());
            if !MEMORY_INFO_FAILURE_REPORTED.swap(true, Ordering::AcqRel) {
                let bounded: String = details.chars().take(8192).collect();
                tracing::warn!(
                    cause = %bounded,
                    "Android memory-info JNI sampling failed; memory guard will use procfs fallback"
                );
            }
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            None
        }
    }
}

/// Captures the stack trace of a Java exception before clearing it from the attached thread.
fn pending_stack_trace(env: &mut JNIEnv) -> Option<String> {
    let throwable = match env.exception_occurred() {
        Ok(throwable) => JObject::from(throwable),
        Err(_) => {
            let _ = env.exception_clear();
            return None;
        }
    };
    if throwable.is_null() {
        let _ = env.exception_clear();
        return None;
    }
    let _ = env.exception_clear();
    let value = match env
        .call_static_method(
            "android/util/Log",
            "getStackTraceString",
            "(Ljava/lang/Throwable;)Ljava/lang/String;",
            &[(&throwable).into()],
        )
        .and_then(|value| value.l())
    {
        Ok(value) => value,
        Err(_) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            return None;
        }
    };
    if value.is_null() {
        return None;
    }
    let details = match env.get_string(&JString::from(value)) {
        Ok(details) => details.into(),
        Err(_) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            return None;
        }
    };
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    Some(details)
}
