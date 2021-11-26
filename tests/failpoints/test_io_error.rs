// Copyright (c) 2017-present, PingCAP, Inc. Licensed under Apache-2.0.

use std::sync::Arc;

use raft::eraftpb::Entry;
use raft_engine::{Config, Engine, LogBatch, ReadableSize};

use crate::util::*;

#[test]
fn test_file_open_error() {
    let dir = tempfile::Builder::new()
        .prefix("test_file_open_error")
        .tempdir()
        .unwrap();
    let cfg = Config {
        dir: dir.path().to_str().unwrap().to_owned(),
        ..Default::default()
    };

    {
        let _f = FailGuard::new("log_fd::create::err", "return");
        assert!(Engine::open(cfg.clone()).is_err());
    }

    {
        let _f = FailGuard::new("log_fd::open::err", "return");
        let _ = Engine::open(cfg.clone()).unwrap();
        assert!(Engine::open(cfg).is_err());
    }
}

#[test]
fn test_file_read_error() {
    let dir = tempfile::Builder::new()
        .prefix("test_file_read_error")
        .tempdir()
        .unwrap();
    let cfg = Config {
        dir: dir.path().to_str().unwrap().to_owned(),
        ..Default::default()
    };
    let entry = vec![b'x'; 1024];

    let engine = Engine::open(cfg).unwrap();
    // Writing an empty message.
    engine
        .write(&mut generate_batch(1, 0, 1, None), true)
        .unwrap();
    engine
        .write(&mut generate_batch(2, 1, 10, Some(&entry)), true)
        .unwrap();
    let mut kv_batch = LogBatch::default();
    let entry_value = Entry {
        index: 111,
        data: entry.to_vec().into(),
        ..Default::default()
    };
    kv_batch
        .put_message(1, b"k".to_vec(), &entry_value)
        .unwrap();
    engine.write(&mut kv_batch, true).unwrap();

    let mut entries = Vec::new();
    let _f = FailGuard::new("log_fd::read::err", "return");
    engine
        .fetch_entries_to::<MessageExtTyped>(1, 0, 1, None, &mut entries)
        .unwrap();
    engine.get_message::<Entry>(1, b"k".as_ref()).unwrap();
    engine
        .fetch_entries_to::<MessageExtTyped>(2, 1, 10, None, &mut entries)
        .unwrap_err();
}

#[test]
fn test_file_write_error() {
    let dir = tempfile::Builder::new()
        .prefix("test_file_write_error")
        .tempdir()
        .unwrap();
    let cfg = Config {
        dir: dir.path().to_str().unwrap().to_owned(),
        bytes_per_sync: ReadableSize::kb(1024),
        target_file_size: ReadableSize::kb(1024),
        ..Default::default()
    };
    let entry = vec![b'x'; 1024];

    let engine = Engine::open(cfg.clone()).unwrap();
    engine
        .write(&mut generate_batch(1, 1, 2, Some(&entry)), false)
        .unwrap();
    {
        let _f = FailGuard::new("log_fd::write::err", "return");
        engine
            .write(&mut generate_batch(1, 2, 3, Some(&entry)), false)
            .unwrap_err();
    }
    {
        let _f = FailGuard::new("log_fd::sync::err", "return");
        engine
            .write(&mut generate_batch(1, 2, 3, Some(&entry)), false)
            .unwrap();
        assert!(catch_unwind_silent(|| {
            let _ = engine.write(&mut generate_batch(1, 3, 4, Some(&entry)), true);
        })
        .is_err());
    }

    // Internal states are consistent after panics. But outstanding writes are not reverted.
    engine
        .write(&mut generate_batch(2, 1, 2, Some(&entry)), true)
        .unwrap();
    drop(engine);
    let engine = Engine::open(cfg).unwrap();
    assert_eq!(engine.first_index(1).unwrap(), 1);
    assert_eq!(engine.last_index(1).unwrap(), 3);
    assert_eq!(engine.first_index(2).unwrap(), 1);
    assert_eq!(engine.last_index(2).unwrap(), 1);
}

#[test]
fn test_file_rotate_error() {
    let dir = tempfile::Builder::new()
        .prefix("test_file_rotate_error")
        .tempdir()
        .unwrap();
    let cfg = Config {
        dir: dir.path().to_str().unwrap().to_owned(),
        bytes_per_sync: ReadableSize::kb(1024),
        target_file_size: ReadableSize::kb(4),
        ..Default::default()
    };
    let entry = vec![b'x'; 1024];

    let engine = Engine::open(cfg.clone()).unwrap();
    engine
        .write(&mut generate_batch(1, 1, 2, Some(&entry)), false)
        .unwrap();
    engine
        .write(&mut generate_batch(1, 2, 3, Some(&entry)), false)
        .unwrap();
    engine
        .write(&mut generate_batch(1, 3, 4, Some(&entry)), false)
        .unwrap();
    {
        let _f = FailGuard::new("log_fd::create::err", "return");
        assert!(catch_unwind_silent(|| {
            let _ = engine.write(&mut generate_batch(1, 4, 5, Some(&entry)), false);
        })
        .is_err());
    }
    {
        // Fail the second header write.
        let _f = FailGuard::new("log_fd::write::err", "1*off->return");
        assert!(catch_unwind_silent(|| {
            let _ = engine.write(&mut generate_batch(1, 4, 5, Some(&entry)), false);
        })
        .is_err());
    }
    {
        let _f = FailGuard::new("log_fd::sync::err", "return");
        assert!(catch_unwind_silent(|| {
            let _ = engine.write(&mut generate_batch(1, 4, 5, Some(&entry)), false);
        })
        .is_err());
    }

    // Internal states are consistent after panics. But outstanding writes are not reverted.
    engine
        .write(&mut generate_batch(2, 1, 2, Some(&entry)), true)
        .unwrap();
    drop(engine);
    let engine = Engine::open(cfg).unwrap();
    assert_eq!(engine.first_index(1).unwrap(), 1);
    assert_eq!(engine.last_index(1).unwrap(), 4);
    assert_eq!(engine.first_index(2).unwrap(), 1);
    assert_eq!(engine.last_index(2).unwrap(), 1);
}

#[test]
fn test_concurrent_write_error() {
    let dir = tempfile::Builder::new()
        .prefix("test_concurrent_write_error")
        .tempdir()
        .unwrap();
    let cfg = Config {
        dir: dir.path().to_str().unwrap().to_owned(),
        bytes_per_sync: ReadableSize::kb(1024),
        target_file_size: ReadableSize::kb(1024),
        ..Default::default()
    };
    let entry = vec![b'x'; 1024];

    let engine = Arc::new(Engine::open(cfg.clone()).unwrap());
    let mut ctx = ConcurrentWriteContext::new(engine.clone());

    // The second of three writes will fail.
    fail::cfg("log_fd::write::err", "1*off->1*return->off").unwrap();
    let entry_clone = entry.clone();
    ctx.write_ext(move |e| {
        e.write(&mut generate_batch(1, 1, 11, Some(&entry_clone)), false)
            .unwrap();
    });
    let entry_clone = entry.clone();
    ctx.write_ext(move |e| {
        e.write(&mut generate_batch(2, 1, 11, Some(&entry_clone)), false)
            .unwrap_err();
    });
    let entry_clone = entry.clone();
    ctx.write_ext(move |e| {
        e.write(&mut generate_batch(3, 1, 11, Some(&entry_clone)), false)
            .unwrap();
    });
    ctx.join();

    assert_eq!(
        10,
        engine
            .fetch_entries_to::<MessageExtTyped>(1, 1, 11, None, &mut vec![])
            .unwrap()
    );
    assert_eq!(
        0,
        engine
            .fetch_entries_to::<MessageExtTyped>(2, 1, 11, None, &mut vec![])
            .unwrap()
    );
    assert_eq!(
        10,
        engine
            .fetch_entries_to::<MessageExtTyped>(3, 1, 11, None, &mut vec![])
            .unwrap()
    );

    {
        let _f1 = FailGuard::new("log_fd::write::err", "return");
        let _f2 = FailGuard::new("log_fd::truncate::err", "return");
        let entry_clone = entry.clone();
        ctx.write_ext(move |e| {
            catch_unwind_silent(|| {
                e.write(&mut generate_batch(1, 11, 21, Some(&entry_clone)), false)
            })
            .unwrap_err();
        });
        // We don't test followers, their panics are hard to catch.
        ctx.join();
    }

    // Internal states are consistent after panics.
    engine
        .write(&mut generate_batch(1, 11, 21, Some(&entry)), true)
        .unwrap();
    drop(ctx);
    drop(engine);

    let engine = Engine::open(cfg).unwrap();
    assert_eq!(
        20,
        engine
            .fetch_entries_to::<MessageExtTyped>(1, 1, 21, None, &mut vec![])
            .unwrap()
    );
}