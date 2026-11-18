//! An `ObjectReader` over objects made up in tests, standing in for PHP.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use speedyspec_hooks::Value;

use crate::contracts::{ObjectReader, Props};

#[derive(Default)]
pub(crate) struct FakeReader {
    objects: RefCell<HashMap<usize, (String, Props)>>,
}

fn addr(rc: &Rc<dyn Any>) -> usize {
    Rc::as_ptr(rc) as *const () as usize
}

impl FakeReader {
    pub(crate) fn object(&self, class: &str, props: Vec<(&str, Value)>) -> Value {
        let rc: Rc<dyn Any> = Rc::new(0u8);
        let props = props.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        self.objects
            .borrow_mut()
            .insert(addr(&rc), (class.to_string(), props));
        Value::Opaque(rc)
    }
}

impl ObjectReader for FakeReader {
    fn class_of(&self, value: &Value) -> Option<String> {
        let Value::Opaque(rc) = value else {
            return None;
        };
        self.objects.borrow().get(&addr(rc)).map(|o| o.0.clone())
    }
    fn properties_of(&self, value: &Value) -> Option<Vec<(String, Value)>> {
        let Value::Opaque(rc) = value else {
            return None;
        };
        self.objects.borrow().get(&addr(rc)).map(|o| o.1.clone())
    }
}
