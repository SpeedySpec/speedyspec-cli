//! The suite every [`crate::Hooks`] backend must pass.
//!
//! A backend crate calls [`run_all`] from one test, with a factory that returns
//! a fresh, empty backend each time it is called.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::{
    AddOptions, Callback, CallbackError, CallbackId, HookErrorKind, Key, SharedHooks, Value,
};

/// What a backend can do beyond the required semantics.
pub struct Capabilities {
    /// True if an `Opaque` value survives a round trip through the backend
    /// (same-process backends). False if it must be refused with `NotPortable`.
    pub carries_opaque: bool,
}

type Make<'a> = &'a dyn Fn() -> SharedHooks;
type Case = (&'static str, fn(Make));

/// Runs every case against a fresh backend from `make`; panics with the case
/// name on the first failure.
pub fn run_all(make: &dyn Fn() -> SharedHooks, capabilities: Capabilities) {
    let cases: &[Case] = &[
        (
            "runs_in_priority_then_insertion_order",
            runs_in_priority_then_insertion_order,
        ),
        (
            "filter_threads_value_through_callbacks",
            filter_threads_value_through_callbacks,
        ),
        (
            "filter_with_no_callbacks_returns_input",
            filter_with_no_callbacks_returns_input,
        ),
        ("accepted_args_truncates", accepted_args_truncates),
        ("remove_by_id_and_by_key", remove_by_id_and_by_key),
        (
            "remove_keyed_needs_matching_priority",
            remove_keyed_needs_matching_priority,
        ),
        ("has_reflects_registration", has_reflects_registration),
        (
            "ran_counts_runs_even_when_empty",
            ran_counts_runs_even_when_empty,
        ),
        (
            "error_stops_firing_and_names_owner",
            error_stops_firing_and_names_owner,
        ),
        ("callback_may_add_during_run", callback_may_add_during_run),
        (
            "callback_may_remove_itself_during_run",
            callback_may_remove_itself_during_run,
        ),
        (
            "callback_may_fire_same_hook_recursively",
            callback_may_fire_same_hook_recursively,
        ),
        ("array_value_round_trips", array_value_round_trips),
        (
            "extreme_priorities_order_and_remove",
            extreme_priorities_order_and_remove,
        ),
    ];
    for (name, case) in cases {
        // Name the case on failure; the assertion message alone would not.
        if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| case(make))) {
            eprintln!("conformance case failed: {name}");
            std::panic::resume_unwind(panic);
        }
    }
    let name = "opaque_value";
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        opaque_value(make, &capabilities)
    })) {
        eprintln!("conformance case failed: {name}");
        std::panic::resume_unwind(panic);
    }
}

type Log = Rc<RefCell<Vec<String>>>;

fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

/// A callback that appends `label` to `log` and returns Null.
fn recorder(log: &Log, label: &str) -> Rc<Callback> {
    let (log, label) = (Rc::clone(log), label.to_owned());
    Rc::new(move |_| {
        log.borrow_mut().push(label.clone());
        Ok(Value::Null)
    })
}

fn at(priority: i64) -> AddOptions {
    AddOptions {
        priority,
        ..AddOptions::default()
    }
}

fn keyed(key: &str, priority: i64) -> AddOptions {
    AddOptions {
        priority,
        key: Some(key.to_owned()),
        ..AddOptions::default()
    }
}

/// add p20 "c", p10 "a", p10 "b"; run; log == ["a","b","c"]
fn runs_in_priority_then_insertion_order(make: Make) {
    let hooks = make();
    let log = log();
    hooks.add("h", recorder(&log, "c"), at(20)).unwrap();
    hooks.add("h", recorder(&log, "a"), at(10)).unwrap();
    hooks.add("h", recorder(&log, "b"), at(10)).unwrap();
    hooks.run("h", &[]).unwrap();
    assert_eq!(*log.borrow(), ["a", "b", "c"]);
}

/// +1 at p10, *2 at p20; filter(Int(3)) == Int(8)
fn filter_threads_value_through_callbacks(make: Make) {
    let hooks = make();
    let plus_one: Rc<Callback> = Rc::new(|args| match args[0] {
        Value::Int(n) => Ok(Value::Int(n + 1)),
        _ => Err(CallbackError("not an int".into())),
    });
    let double: Rc<Callback> = Rc::new(|args| match args[0] {
        Value::Int(n) => Ok(Value::Int(n * 2)),
        _ => Err(CallbackError("not an int".into())),
    });
    // Registered out of order so priority, not insertion, decides: (3+1)*2 = 8.
    hooks.add("f", double, at(20)).unwrap();
    hooks.add("f", plus_one, at(10)).unwrap();
    assert_eq!(
        hooks.filter("f", Value::Int(3), &[]).unwrap(),
        Value::Int(8)
    );
}

/// filter("none", Int(3), &[]) == Int(3)
fn filter_with_no_callbacks_returns_input(make: Make) {
    let hooks = make();
    assert_eq!(
        hooks.filter("none", Value::Int(3), &[]).unwrap(),
        Value::Int(3)
    );
}

/// accepted_args 1; run with 3 args; callback saw len 1
fn accepted_args_truncates(make: Make) {
    let hooks = make();
    let seen = Rc::new(Cell::new(usize::MAX));
    let seen_in = Rc::clone(&seen);
    let cb: Rc<Callback> = Rc::new(move |args| {
        seen_in.set(args.len());
        Ok(Value::Null)
    });
    let options = AddOptions {
        accepted_args: 1,
        ..AddOptions::default()
    };
    hooks.add("h", cb, options).unwrap();
    hooks
        .run("h", &[Value::Int(1), Value::Int(2), Value::Int(3)])
        .unwrap();
    assert_eq!(seen.get(), 1);
}

/// both return Ok(true) once, Ok(false) the second time
fn remove_by_id_and_by_key(make: Make) {
    let hooks = make();
    let log = log();
    let id = hooks.add("h", recorder(&log, "a"), at(10)).unwrap();
    assert_eq!(hooks.remove("h", id), Ok(true));
    assert_eq!(hooks.remove("h", id), Ok(false));

    hooks.add("h", recorder(&log, "b"), keyed("k", 10)).unwrap();
    assert_eq!(hooks.remove_keyed("h", "k", 10), Ok(true));
    assert_eq!(hooks.remove_keyed("h", "k", 10), Ok(false));

    // Removed callbacks really are gone.
    hooks.run("h", &[]).unwrap();
    assert!(log.borrow().is_empty());
}

/// added at 10; remove_keyed(.., 20) == Ok(false); has_keyed Ok(true)
fn remove_keyed_needs_matching_priority(make: Make) {
    let hooks = make();
    let log = log();
    hooks.add("h", recorder(&log, "a"), keyed("k", 10)).unwrap();
    assert_eq!(hooks.remove_keyed("h", "k", 20), Ok(false));
    assert_eq!(hooks.has_keyed("h", "k"), Ok(true));
}

/// false, add, true, remove, false
fn has_reflects_registration(make: Make) {
    let hooks = make();
    let log = log();
    assert_eq!(hooks.has("h"), Ok(false));
    let id = hooks.add("h", recorder(&log, "a"), at(10)).unwrap();
    assert_eq!(hooks.has("h"), Ok(true));
    hooks.remove("h", id).unwrap();
    assert_eq!(hooks.has("h"), Ok(false));
}

/// run("x") twice with nothing registered; ran("x") == Ok(2)
fn ran_counts_runs_even_when_empty(make: Make) {
    let hooks = make();
    hooks.run("x", &[]).unwrap();
    hooks.run("x", &[]).unwrap();
    assert_eq!(hooks.ran("x"), Ok(2));
    assert_eq!(hooks.ran("never"), Ok(0));
}

/// 2nd of 3 errs with owner "acme/p"; 3rd not called; kind Callback,
/// err.hook == "h", err.owner == Some("acme/p")
fn error_stops_firing_and_names_owner(make: Make) {
    let hooks = make();
    let log = log();
    hooks.add("h", recorder(&log, "first"), at(10)).unwrap();
    let failing: Rc<Callback> = Rc::new(|_| Err(CallbackError("boom".into())));
    hooks
        .add(
            "h",
            failing,
            AddOptions {
                priority: 11,
                owner: Some("acme/p".into()),
                ..AddOptions::default()
            },
        )
        .unwrap();
    hooks.add("h", recorder(&log, "third"), at(12)).unwrap();

    let err = hooks.run("h", &[]).unwrap_err();
    assert_eq!(err.kind, HookErrorKind::Callback);
    assert_eq!(err.hook, "h");
    assert_eq!(err.owner.as_deref(), Some("acme/p"));
    assert_eq!(*log.borrow(), ["first"]);
}

/// callback adds another to same hook; first run calls 1, second 2
fn callback_may_add_during_run(make: Make) {
    let hooks = make();
    let calls = Rc::new(Cell::new(0u32));
    let added = Rc::new(Cell::new(false));
    let (h, calls_in, added_in) = (Rc::clone(&hooks), Rc::clone(&calls), Rc::clone(&added));
    let adder: Rc<Callback> = Rc::new(move |_| {
        calls_in.set(calls_in.get() + 1);
        if !added_in.replace(true) {
            let calls_inner = Rc::clone(&calls_in);
            let second: Rc<Callback> = Rc::new(move |_| {
                calls_inner.set(calls_inner.get() + 1);
                Ok(Value::Null)
            });
            h.add("h", second, at(10)).unwrap();
        }
        Ok(Value::Null)
    });
    hooks.add("h", adder, at(10)).unwrap();

    hooks.run("h", &[]).unwrap();
    assert_eq!(calls.get(), 1, "an addition applies from the next firing");
    calls.set(0);
    hooks.run("h", &[]).unwrap();
    assert_eq!(calls.get(), 2);
}

/// removes itself; run completes; next run calls 0
fn callback_may_remove_itself_during_run(make: Make) {
    let hooks = make();
    let calls = Rc::new(Cell::new(0u32));
    let id: Rc<Cell<Option<CallbackId>>> = Rc::new(Cell::new(None));
    let (h, calls_in, id_in) = (Rc::clone(&hooks), Rc::clone(&calls), Rc::clone(&id));
    let cb: Rc<Callback> = Rc::new(move |_| {
        calls_in.set(calls_in.get() + 1);
        h.remove("h", id_in.get().unwrap()).unwrap();
        Ok(Value::Null)
    });
    id.set(Some(hooks.add("h", cb, at(10)).unwrap()));

    hooks.run("h", &[]).unwrap();
    assert_eq!(calls.get(), 1);
    hooks.run("h", &[]).unwrap();
    assert_eq!(calls.get(), 1, "the removed callback is not called again");
}

/// recurses to depth 3 via a counter; no panic; ran == 3
fn callback_may_fire_same_hook_recursively(make: Make) {
    let hooks = make();
    let depth = Rc::new(Cell::new(0u32));
    let (h, depth_in) = (Rc::clone(&hooks), Rc::clone(&depth));
    let cb: Rc<Callback> = Rc::new(move |_| {
        depth_in.set(depth_in.get() + 1);
        if depth_in.get() < 3 {
            h.run("h", &[]).map_err(|e| CallbackError(e.message))?;
        }
        Ok(Value::Null)
    });
    hooks.add("h", cb, at(10)).unwrap();

    hooks.run("h", &[]).unwrap();
    assert_eq!(depth.get(), 3);
    assert_eq!(hooks.ran("h"), Ok(3));
}

/// i64::MAX runs last, i64::MIN first; remove_keyed needs the exact extreme
fn extreme_priorities_order_and_remove(make: Make) {
    let hooks = make();
    let log = log();
    hooks
        .add("h", recorder(&log, "last"), keyed("max", i64::MAX))
        .unwrap();
    hooks.add("h", recorder(&log, "middle"), at(10)).unwrap();
    hooks
        .add("h", recorder(&log, "first"), keyed("min", i64::MIN))
        .unwrap();
    hooks
        .add("h", recorder(&log, "also last"), at(i64::MAX))
        .unwrap();
    hooks.run("h", &[]).unwrap();
    assert_eq!(*log.borrow(), ["first", "middle", "last", "also last"]);

    assert_eq!(hooks.remove_keyed("h", "max", i64::MIN), Ok(false));
    assert_eq!(hooks.remove_keyed("h", "max", i64::MAX), Ok(true));
    assert_eq!(hooks.remove_keyed("h", "min", i64::MAX), Ok(false));
    assert_eq!(hooks.remove_keyed("h", "min", i64::MIN), Ok(true));
    log.borrow_mut().clear();
    hooks.run("h", &[]).unwrap();
    assert_eq!(*log.borrow(), ["middle", "also last"]);
}

/// nested Array with Int and Str keys through an identity filter; equal
fn array_value_round_trips(make: Make) {
    let hooks = make();
    let identity: Rc<Callback> = Rc::new(|args| Ok(args[0].clone()));
    hooks.add("f", identity, at(10)).unwrap();
    let value = Value::Array(vec![
        (Key::Int(7), Value::Str("seven".into())),
        (Key::Str("7".into()), Value::Float(7.0)),
        (
            Key::Str("nested".into()),
            Value::Array(vec![
                (Key::Int(1), Value::Null),
                (Key::Int(0), Value::Bytes(vec![0xff])),
            ]),
        ),
    ]);
    assert_eq!(hooks.filter("f", value.clone(), &[]).unwrap(), value);
}

/// carries_opaque: identity filter returns the same Rc (Rc::ptr_eq).
/// otherwise: filter with an Opaque is Err with kind NotPortable
fn opaque_value(make: Make, capabilities: &Capabilities) {
    let hooks = make();
    let identity: Rc<Callback> = Rc::new(|args| Ok(args[0].clone()));
    hooks.add("f", identity, at(10)).unwrap();
    let payload: Rc<dyn std::any::Any> = Rc::new(42u32);
    let result = hooks.filter("f", Value::Opaque(Rc::clone(&payload)), &[]);
    if capabilities.carries_opaque {
        match result.unwrap() {
            Value::Opaque(back) => assert!(Rc::ptr_eq(&back, &payload)),
            other => panic!("expected the Opaque back, got {other:?}"),
        }
    } else {
        assert_eq!(result.unwrap_err().kind, HookErrorKind::NotPortable);
    }
}
