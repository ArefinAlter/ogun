//! JNI exports for `dev.verne.ogun.gateway.NativeGateway` (Android only).
//!
//! This is the crate's one platform module that needs `unsafe_code` to be allowed: JNI looks the functions up by
//! their unmangled names (`#[unsafe(no_mangle)]`), which the `unsafe_code` lint flags. The functions themselves
//! use only the safe `jni` wrappers and forward to [`crate::ffi`]; no raw pointer is dereferenced here. A Java
//! string that cannot be read becomes an `api.invalid_params` envelope, and a result that cannot be returned
//! becomes a null `String`, which the Kotlin side treats as an internal error.
#![allow(unsafe_code)]

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jint, jlong, jstring};

use crate::ffi;

const BAD_STRING: &str = r#"{"error":{"code":"api.invalid_params","class":"invalid_request","message":"unreadable string from Java","retryable":false}}"#;

fn read(env: &mut JNIEnv<'_>, s: &JString<'_>) -> Option<String> {
    env.get_string(s).ok().map(Into::into)
}

fn out(env: &JNIEnv<'_>, text: &str) -> jstring {
    env.new_string(text)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// `NativeGateway.open(dir, configJson)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_verne_ogun_gateway_NativeGateway_open<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    dir: JString<'l>,
    config: JString<'l>,
) -> jstring {
    let result = match (read(&mut env, &dir), read(&mut env, &config)) {
        (Some(d), Some(c)) => ffi::open(&d, &c),
        _ => BAD_STRING.to_owned(),
    };
    out(&env, &result)
}

/// `NativeGateway.close()`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_verne_ogun_gateway_NativeGateway_close<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jstring {
    out(&env, &ffi::close())
}

/// `NativeGateway.append(batchJson)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_verne_ogun_gateway_NativeGateway_append<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    batch: JString<'l>,
) -> jstring {
    let result = read(&mut env, &batch).map_or_else(|| BAD_STRING.to_owned(), |b| ffi::append(&b));
    out(&env, &result)
}

/// `NativeGateway.appendBody(origin, receivedMs, body, ndjson)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_verne_ogun_gateway_NativeGateway_appendBody<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    origin: JString<'l>,
    received_ms: jlong,
    body: JString<'l>,
    ndjson: jboolean,
) -> jstring {
    let result = match (read(&mut env, &origin), read(&mut env, &body)) {
        (Some(o), Some(b)) => ffi::append_body(&o, received_ms, &b, ndjson != 0),
        _ => BAD_STRING.to_owned(),
    };
    out(&env, &result)
}

/// `NativeGateway.seal(nowMs)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_verne_ogun_gateway_NativeGateway_seal<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
    now_ms: jlong,
) -> jstring {
    out(&env, &ffi::seal(now_ms))
}

/// `NativeGateway.offer()`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_verne_ogun_gateway_NativeGateway_offer<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jstring {
    out(&env, &ffi::offer())
}

/// `NativeGateway.applyWant(wantJson)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_verne_ogun_gateway_NativeGateway_applyWant<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    want: JString<'l>,
) -> jstring {
    let result =
        read(&mut env, &want).map_or_else(|| BAD_STRING.to_owned(), |w| ffi::apply_want(&w));
    out(&env, &result)
}

/// `NativeGateway.chunk(blake3, index)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_verne_ogun_gateway_NativeGateway_chunk<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    digest: JString<'l>,
    index: jint,
) -> jstring {
    let result = match (read(&mut env, &digest), u32::try_from(index)) {
        (Some(d), Ok(i)) => ffi::chunk(&d, i),
        _ => BAD_STRING.to_owned(),
    };
    out(&env, &result)
}

/// `NativeGateway.recordSync(outcomeJson)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_verne_ogun_gateway_NativeGateway_recordSync<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    outcome: JString<'l>,
) -> jstring {
    let result =
        read(&mut env, &outcome).map_or_else(|| BAD_STRING.to_owned(), |o| ffi::record_sync(&o));
    out(&env, &result)
}

/// `NativeGateway.status()`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_verne_ogun_gateway_NativeGateway_status<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jstring {
    out(&env, &ffi::status())
}
