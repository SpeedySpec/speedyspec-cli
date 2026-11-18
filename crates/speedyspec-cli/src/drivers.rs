//! The install drivers compiled into this binary.

use std::rc::Rc;

use speedyspec_driver::Driver;
use speedyspec_driver_zip::ZipDriver;

/// Every driver this binary can run.
pub fn all() -> Vec<Rc<dyn Driver>> {
    vec![Rc::new(ZipDriver::new())]
}

/// Their ids, without building them.
pub fn ids() -> Vec<&'static str> {
    vec!["zip"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_the_drivers() {
        let built: Vec<&str> = all().iter().map(|driver| driver.id()).collect();
        assert_eq!(built, ids());
    }
}
