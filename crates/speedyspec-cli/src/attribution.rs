//! A [`Hooks`] decorator that knows which plugin every callback belongs to, so a value one
//! plugin's callback returns can be checked and blamed on exactly that plugin.
//!
//! Every callback is wrapped when it is added. The wrapper:
//!
//! - marks its owner as the one *running* in the current execution context while the callback
//!   runs, so a hook the callback adds itself (say from `speedyspec_init`) belongs to the same
//!   plugin;
//! - on a hook with a registered [`Validator`], when the hook is being filtered, checks what the
//!   callback returned against what it was given. Problems are recorded as [`Rejection`]s
//!   naming that callback's owner, and the validator decides what continues down the chain
//!   (the previous value when the whole return value is unusable), so a bad plugin cannot spoil
//!   the value for later plugins or the final result.
//!
//! The bookkeeping is per execution context (the main context and each PHP fiber), because a
//! callback running in a fiber can suspend mid-firing while other code adds hooks or fires
//! filters, and resume later. On one context's stack calls nest strictly, so saving and
//! restoring around each call, per context, stays correct across suspension; across contexts
//! nothing is shared. The context comes from a function given at construction
//! (`speedyspec_php::execution_context` in the binary). No `RefCell` borrow is held while a
//! callback or validator runs.
//!
//! One known gap: a fiber that PHP abandons without unwinding it (only after a fatal error)
//! never restores its entries, and a later fiber whose context lands at the same address
//! inherits its "running owner" for hooks it adds outside any callback. That only changes which
//! plugin an error message names, and only on an engine already poisoned by a fatal error.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use speedyspec_hooks::{AddOptions, Callback, CallbackId, HookError, Hooks, SharedHooks, Value};

/// What a [`Validator`] made of one callback's return value.
pub struct Checked {
    /// What continues down the chain in place of the return value.
    pub value: Value,
    /// Problems to report against the callback's owner, each `<field>: <problem>` or just
    /// `<problem>`.
    pub problems: Vec<String>,
}

/// Checks what one callback returned on a filter: `(value it was given, value it returned)`.
pub type Validator = dyn Fn(&Value, Value) -> Checked;

/// A problem a validator found in one callback's return value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    /// The owner of the callback that returned it; `None` when the callback had none.
    pub owner: Option<String>,
    pub hook: String,
    pub problem: String,
}

/// Names the execution context running now; see the module docs.
pub type ContextFn = fn() -> usize;

/// What the wrappers share with the decorator. Holds no hooks, so a wrapper (stored in the inner
/// backend) does not keep the backend alive.
struct State {
    context: ContextFn,
    /// The owner of the wrapped callback running now, per context.
    running: RefCell<HashMap<usize, String>>,
    /// Firings through this decorator in progress, per context, innermost last: true for a
    /// filter, false for an action.
    firings: RefCell<HashMap<usize, Vec<bool>>>,
    validators: RefCell<HashMap<String, Rc<Validator>>>,
    rejections: RefCell<Vec<Rejection>>,
}

impl State {
    fn running_owner(&self, context: usize) -> Option<String> {
        self.running.borrow().get(&context).cloned()
    }

    /// True when the innermost firing in `context` is a filter.
    fn filtering(&self, context: usize) -> bool {
        self.firings
            .borrow()
            .get(&context)
            .and_then(|stack| stack.last().copied())
            .unwrap_or(false)
    }
}

/// Sets the running owner of one context and puts the previous one back when dropped, also when
/// the callback panics.
struct Running<'a> {
    state: &'a State,
    context: usize,
    previous: Option<String>,
}

impl<'a> Running<'a> {
    fn enter(state: &'a State, context: usize, owner: Option<String>) -> Self {
        let previous = {
            let mut running = state.running.borrow_mut();
            match owner {
                Some(owner) => running.insert(context, owner),
                None => running.remove(&context),
            }
        };
        Self {
            state,
            context,
            previous,
        }
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut running = self.state.running.borrow_mut();
        match self.previous.take() {
            Some(owner) => {
                running.insert(self.context, owner);
            }
            None => {
                running.remove(&self.context);
            }
        }
    }
}

/// One firing in progress in one context; popped when dropped.
struct Firing<'a> {
    state: &'a State,
    context: usize,
}

impl<'a> Firing<'a> {
    fn enter(state: &'a State, filter: bool) -> Self {
        let context = (state.context)();
        state
            .firings
            .borrow_mut()
            .entry(context)
            .or_default()
            .push(filter);
        Self { state, context }
    }
}

impl Drop for Firing<'_> {
    fn drop(&mut self) {
        let mut firings = self.state.firings.borrow_mut();
        if let Some(stack) = firings.get_mut(&self.context) {
            stack.pop();
            if stack.is_empty() {
                firings.remove(&self.context);
            }
        }
    }
}

/// The decorator. Forwards every [`Hooks`] method to `inner`, wrapping callbacks on the way in.
pub struct AttributedHooks {
    inner: SharedHooks,
    state: Rc<State>,
}

impl AttributedHooks {
    pub fn new(inner: SharedHooks, context: ContextFn) -> Self {
        Self {
            inner,
            state: Rc::new(State {
                context,
                running: RefCell::default(),
                firings: RefCell::default(),
                validators: RefCell::default(),
                rejections: RefCell::default(),
            }),
        }
    }

    /// Checks every callback's return value on `hook` with `validator` from now on, whenever
    /// `hook` is filtered. Replaces an earlier validator for the same hook.
    pub fn validate(&self, hook: &str, validator: Rc<Validator>) {
        self.state
            .validators
            .borrow_mut()
            .insert(hook.to_owned(), validator);
    }

    /// Forgets every validator (and whatever they hold).
    pub fn clear_validators(&self) {
        let validators = std::mem::take(&mut *self.state.validators.borrow_mut());
        drop(validators);
    }

    /// The problems validators found since the last call, in the order they were found.
    pub fn take_rejections(&self) -> Vec<Rejection> {
        std::mem::take(&mut *self.state.rejections.borrow_mut())
    }
}

/// Wraps `callback`, registered on `hook` for `owner`, which sees `accepted_args` arguments.
fn wrap(
    state: Rc<State>,
    hook: String,
    owner: Option<String>,
    accepted_args: usize,
    callback: Rc<Callback>,
) -> Rc<Callback> {
    Rc::new(move |args: &[Value]| {
        let context = (state.context)();
        let returned = {
            let _running = Running::enter(&state, context, owner.clone());
            callback(&args[..accepted_args.min(args.len())])
        }?;
        // The callback may have suspended and resumed: it is back on its own stack, so
        // `context` and the innermost firing in it are its own again.
        if !state.filtering(context) {
            return Ok(returned);
        }
        let validator = state.validators.borrow().get(&hook).cloned();
        let (Some(validator), Some(given)) = (validator, args.first()) else {
            return Ok(returned);
        };
        let checked = validator(given, returned);
        if !checked.problems.is_empty() {
            let mut rejections = state.rejections.borrow_mut();
            for problem in checked.problems {
                rejections.push(Rejection {
                    owner: owner.clone(),
                    hook: hook.clone(),
                    problem,
                });
            }
        }
        Ok(checked.value)
    })
}

impl Hooks for AttributedHooks {
    /// The owner is the running callback's in this context, if one is running (a hook added by
    /// a plugin's callback is that plugin's), else `options.owner`. The inner backend is asked
    /// for every argument (`accepted_args` is applied by the wrapper), so the wrapper can see
    /// the value a filter was given.
    fn add(
        &self,
        hook: &str,
        callback: Rc<Callback>,
        options: AddOptions,
    ) -> Result<CallbackId, HookError> {
        let owner = self
            .state
            .running_owner((self.state.context)())
            .or(options.owner);
        let wrapped = wrap(
            Rc::clone(&self.state),
            hook.to_owned(),
            owner.clone(),
            options.accepted_args,
            callback,
        );
        self.inner.add(
            hook,
            wrapped,
            AddOptions {
                priority: options.priority,
                accepted_args: usize::MAX,
                key: options.key,
                owner,
            },
        )
    }

    fn remove(&self, hook: &str, id: CallbackId) -> Result<bool, HookError> {
        self.inner.remove(hook, id)
    }

    fn remove_keyed(&self, hook: &str, key: &str, priority: i64) -> Result<bool, HookError> {
        self.inner.remove_keyed(hook, key, priority)
    }

    fn has(&self, hook: &str) -> Result<bool, HookError> {
        self.inner.has(hook)
    }

    fn has_keyed(&self, hook: &str, key: &str) -> Result<bool, HookError> {
        self.inner.has_keyed(hook, key)
    }

    fn run(&self, hook: &str, args: &[Value]) -> Result<(), HookError> {
        let _firing = Firing::enter(&self.state, false);
        self.inner.run(hook, args)
    }

    fn filter(&self, hook: &str, value: Value, args: &[Value]) -> Result<Value, HookError> {
        let _firing = Firing::enter(&self.state, true);
        self.inner.filter(hook, value, args)
    }

    fn ran(&self, hook: &str) -> Result<u64, HookError> {
        self.inner.ran(hook)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use speedyspec_hooks::conformance::{Capabilities, run_all};
    use speedyspec_hooks::{CallbackError, HookErrorKind, Key};
    use speedyspec_hooks_memory::MemoryHooks;

    use super::*;

    thread_local! {
        /// The fake execution context the tests switch between, as fibers would.
        static CONTEXT: Cell<usize> = const { Cell::new(1) };
    }

    fn context() -> usize {
        CONTEXT.with(Cell::get)
    }

    fn switch_to(context: usize) {
        CONTEXT.with(|c| c.set(context));
    }

    fn hooks() -> Rc<AttributedHooks> {
        switch_to(1);
        Rc::new(AttributedHooks::new(Rc::new(MemoryHooks::new()), context))
    }

    fn owned(owner: &str) -> AddOptions {
        AddOptions {
            owner: Some(owner.to_owned()),
            ..AddOptions::default()
        }
    }

    fn list(items: &[i64]) -> Value {
        Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, n)| (Key::Int(i as i64), Value::Int(*n)))
                .collect(),
        )
    }

    /// Accepts a list of non-negative ints, dropping negative ones; anything else is replaced by
    /// what the callback was given.
    fn non_negative() -> Rc<Validator> {
        Rc::new(|given: &Value, returned: Value| {
            let Value::Array(items) = returned else {
                return Checked {
                    value: given.clone(),
                    problems: vec!["expected a list".to_owned()],
                };
            };
            let mut kept = Vec::new();
            let mut problems = Vec::new();
            for (i, (_, item)) in items.into_iter().enumerate() {
                match item {
                    Value::Int(n) if n >= 0 => kept.push((Key::Int(kept.len() as i64), item)),
                    _ => problems.push(format!("[{i}]: negative")),
                }
            }
            Checked {
                value: Value::Array(kept),
                problems,
            }
        })
    }

    fn appender(n: i64) -> Rc<Callback> {
        Rc::new(move |args: &[Value]| {
            let Some(Value::Array(items)) = args.first() else {
                return Err(CallbackError("not a list".into()));
            };
            let mut items = items.clone();
            items.push((Key::Int(items.len() as i64), Value::Int(n)));
            Ok(Value::Array(items))
        })
    }

    #[test]
    fn conforms_to_the_hook_contract() {
        run_all(
            &|| hooks() as SharedHooks,
            Capabilities {
                carries_opaque: true,
            },
        );
    }

    #[test]
    fn rejection_names_the_owner_and_later_plugins_are_unaffected() {
        let hooks = hooks();
        hooks.add("f", appender(1), owned("good/a")).unwrap();
        hooks
            .add(
                "f",
                Rc::new(|_| Ok(Value::Str("nope".into()))),
                owned("bad/b"),
            )
            .unwrap();
        hooks.add("f", appender(-5), owned("bad/c")).unwrap();
        hooks.add("f", appender(3), owned("good/d")).unwrap();
        hooks.validate("f", non_negative());

        let value = hooks.filter("f", list(&[]), &[]).unwrap();
        assert_eq!(value, list(&[1, 3]));
        assert_eq!(
            hooks.take_rejections(),
            [
                Rejection {
                    owner: Some("bad/b".into()),
                    hook: "f".into(),
                    problem: "expected a list".into(),
                },
                Rejection {
                    owner: Some("bad/c".into()),
                    hook: "f".into(),
                    problem: "[1]: negative".into(),
                },
            ]
        );
        assert!(hooks.take_rejections().is_empty());
    }

    #[test]
    fn actions_and_unvalidated_hooks_are_not_checked() {
        let hooks = hooks();
        hooks.add("f", appender(-1), owned("p")).unwrap();
        hooks.validate("f", non_negative());
        // Fired as an action, the return value means nothing.
        hooks.run("f", &[list(&[])]).unwrap();
        assert!(hooks.take_rejections().is_empty());
        // Another hook has no validator.
        hooks.add("g", appender(-1), owned("p")).unwrap();
        assert_eq!(hooks.filter("g", list(&[]), &[]).unwrap(), list(&[-1]));
        assert!(hooks.take_rejections().is_empty());
        // Once the validators are cleared, nothing is checked.
        hooks.clear_validators();
        assert_eq!(hooks.filter("f", list(&[]), &[]).unwrap(), list(&[-1]));
        assert!(hooks.take_rejections().is_empty());
    }

    #[test]
    fn hooks_a_callback_adds_belong_to_its_owner() {
        let hooks = hooks();
        let inner = Rc::clone(&hooks);
        let registrar: Rc<Callback> = Rc::new(move |_| {
            // Added with no owner of its own, as PHP does after loading.
            inner
                .add("f", appender(-1), AddOptions::default())
                .map_err(|e| CallbackError(e.message))?;
            Ok(Value::Null)
        });
        hooks.add("init", registrar, owned("late/p")).unwrap();
        hooks.run("init", &[]).unwrap();
        hooks.validate("f", non_negative());
        hooks.filter("f", list(&[]), &[]).unwrap();
        let rejections = hooks.take_rejections();
        assert_eq!(rejections.len(), 1);
        assert_eq!(rejections[0].owner.as_deref(), Some("late/p"));

        // Outside any callback the owner is the caller's, or none.
        hooks.add("g", appender(-1), AddOptions::default()).unwrap();
        hooks.validate("g", non_negative());
        hooks.filter("g", list(&[]), &[]).unwrap();
        assert_eq!(hooks.take_rejections()[0].owner, None);
    }

    #[test]
    fn callback_error_names_the_owner() {
        let hooks = hooks();
        hooks
            .add(
                "f",
                Rc::new(|_| Err(CallbackError("boom".into()))),
                owned("acme/p"),
            )
            .unwrap();
        let err = hooks.filter("f", Value::Null, &[]).unwrap_err();
        assert_eq!(err.kind, HookErrorKind::Callback);
        assert_eq!(err.owner.as_deref(), Some("acme/p"));
        assert_eq!(err.message, "boom");
    }

    #[test]
    fn owner_is_restored_after_a_panicking_callback() {
        let hooks = hooks();
        hooks
            .add("p", Rc::new(|_| panic!("callback panics")), owned("x/p"))
            .unwrap();
        let fired = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = hooks.run("p", &[]);
        }));
        assert!(fired.is_err());
        assert!(hooks.state.running.borrow().is_empty());
        assert!(hooks.state.firings.borrow().is_empty());
    }

    #[test]
    fn re_entrant_filters_are_checked_each() {
        let hooks = hooks();
        let inner = Rc::clone(&hooks);
        let depth = Rc::new(Cell::new(0));
        let depth_in = Rc::clone(&depth);
        let recursive: Rc<Callback> = Rc::new(move |args: &[Value]| {
            depth_in.set(depth_in.get() + 1);
            if depth_in.get() == 1 {
                // The nested filter's bad value is dropped by its own check.
                let nested = inner
                    .filter("f", list(&[]), &[])
                    .map_err(|e| CallbackError(e.message))?;
                assert_eq!(nested, list(&[]));
            }
            Ok(args[0].clone())
        });
        hooks.add("f", recursive, owned("outer/p")).unwrap();
        hooks.add("f", appender(-1), owned("bad/p")).unwrap();
        hooks.validate("f", non_negative());
        assert_eq!(hooks.filter("f", list(&[]), &[]).unwrap(), list(&[]));
        let owners: Vec<_> = hooks
            .take_rejections()
            .into_iter()
            .map(|r| r.owner.unwrap())
            .collect();
        assert_eq!(owners, ["bad/p", "bad/p"]);
    }

    #[test]
    fn wrapped_callbacks_can_be_removed_by_id_and_by_key() {
        let hooks = hooks();
        let calls = Rc::new(Cell::new(0));
        let counter = |calls: &Rc<Cell<i32>>| -> Rc<Callback> {
            let calls = Rc::clone(calls);
            Rc::new(move |_| {
                calls.set(calls.get() + 1);
                Ok(Value::Null)
            })
        };
        let id = hooks.add("h", counter(&calls), owned("p")).unwrap();
        hooks
            .add(
                "h",
                counter(&calls),
                AddOptions {
                    key: Some("k".into()),
                    priority: i64::MAX,
                    ..owned("p")
                },
            )
            .unwrap();
        assert_eq!(hooks.has_keyed("h", "k"), Ok(true));
        hooks.run("h", &[]).unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(hooks.remove("h", id), Ok(true));
        assert_eq!(hooks.remove_keyed("h", "k", 10), Ok(false));
        assert_eq!(hooks.remove_keyed("h", "k", i64::MAX), Ok(true));
        assert_eq!(hooks.has("h"), Ok(false));
        hooks.run("h", &[]).unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(hooks.ran("h"), Ok(2));
    }

    /// A callback that "suspends" (the context switches away mid-call, as a PHP fiber does)
    /// must not lend its owner to the other context, and gets it back when it resumes.
    #[test]
    fn owner_and_firing_are_per_context() {
        let hooks = hooks();
        let inner = Rc::clone(&hooks);
        let seen: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let seen_in = Rc::clone(&seen);
        let probe = move |inner: &AttributedHooks| inner.state.running_owner(context());
        let in_fiber: Rc<Callback> = Rc::new(move |args: &[Value]| {
            // Running in context 2 (the fiber), owned by fiber/p.
            seen_in.borrow_mut().push(probe(&inner));
            // Suspend: the main context runs, adds and fires.
            switch_to(1);
            seen_in.borrow_mut().push(probe(&inner));
            inner
                .add("main.added", appender(-1), AddOptions::default())
                .unwrap();
            inner.validate("main.added", non_negative());
            inner.filter("main.added", list(&[]), &[]).unwrap();
            // Resume the fiber.
            switch_to(2);
            seen_in.borrow_mut().push(probe(&inner));
            Ok(args[0].clone())
        });
        hooks.add("fiber.f", in_fiber, owned("fiber/p")).unwrap();
        hooks
            .add("fiber.f", appender(-1), owned("fiber/q"))
            .unwrap();
        hooks.validate("fiber.f", non_negative());

        switch_to(2);
        assert_eq!(hooks.filter("fiber.f", list(&[]), &[]).unwrap(), list(&[]));
        switch_to(1);
        assert_eq!(
            *seen.borrow(),
            [Some("fiber/p".to_owned()), None, Some("fiber/p".to_owned())]
        );
        let owners: Vec<_> = hooks
            .take_rejections()
            .into_iter()
            .map(|r| (r.hook, r.owner))
            .collect();
        assert_eq!(
            owners,
            [
                ("main.added".to_owned(), None),
                ("fiber.f".to_owned(), Some("fiber/q".to_owned())),
            ]
        );
        assert!(hooks.state.running.borrow().is_empty());
        assert!(hooks.state.firings.borrow().is_empty());
    }

    #[test]
    fn inner_errors_are_propagated() {
        struct Down;
        impl Hooks for Down {
            fn add(
                &self,
                h: &str,
                _: Rc<Callback>,
                _: AddOptions,
            ) -> Result<CallbackId, HookError> {
                Err(down(h))
            }
            fn remove(&self, h: &str, _: CallbackId) -> Result<bool, HookError> {
                Err(down(h))
            }
            fn remove_keyed(&self, h: &str, _: &str, _: i64) -> Result<bool, HookError> {
                Err(down(h))
            }
            fn has(&self, h: &str) -> Result<bool, HookError> {
                Err(down(h))
            }
            fn has_keyed(&self, h: &str, _: &str) -> Result<bool, HookError> {
                Err(down(h))
            }
            fn run(&self, h: &str, _: &[Value]) -> Result<(), HookError> {
                Err(down(h))
            }
            fn filter(&self, h: &str, _: Value, _: &[Value]) -> Result<Value, HookError> {
                Err(down(h))
            }
            fn ran(&self, h: &str) -> Result<u64, HookError> {
                Err(down(h))
            }
        }
        fn down(hook: &str) -> HookError {
            HookError {
                kind: HookErrorKind::Backend,
                hook: hook.to_owned(),
                owner: None,
                message: "down".to_owned(),
            }
        }
        let hooks = AttributedHooks::new(Rc::new(Down), context);
        let backend =
            |r: Result<(), HookError>| assert_eq!(r.unwrap_err().kind, HookErrorKind::Backend);
        backend(hooks.add("h", appender(1), AddOptions::default()).map(drop));
        backend(hooks.remove("h", CallbackId(0)).map(drop));
        backend(hooks.remove_keyed("h", "k", 1).map(drop));
        backend(hooks.has("h").map(drop));
        backend(hooks.has_keyed("h", "k").map(drop));
        backend(hooks.run("h", &[]));
        backend(hooks.filter("h", Value::Null, &[]).map(drop));
        backend(hooks.ran("h").map(drop));
        assert!(hooks.state.firings.borrow().is_empty());
    }
}
