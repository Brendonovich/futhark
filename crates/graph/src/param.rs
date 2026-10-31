use std::fmt;

/// A parameter value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Float(f64),
    Int(i64),
    Bool(bool),
    /// The name of a menu item.
    Menu(String),
    String(String),
}

impl Value {
    fn describe(&self) -> &'static str {
        match self {
            Value::Float(_) => "a float",
            Value::Int(_) => "an integer",
            Value::Bool(_) => "a toggle",
            Value::Menu(_) => "a menu item",
            Value::String(_) => "a string",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Float(v) => write!(f, "{v:?}"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Bool(v) => write!(f, "{v}"),
            Value::Menu(v) => write!(f, "{v}"),
            Value::String(v) => write!(f, "{v:?}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuItem {
    pub name: &'static str,
    pub label: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    Float,
    Int,
    Toggle,
    Menu(&'static [MenuItem]),
    String,
}

impl ParamKind {
    fn describe(self) -> &'static str {
        match self {
            ParamKind::Float => "a float",
            ParamKind::Int => "an integer",
            ParamKind::Toggle => "a toggle",
            ParamKind::Menu(_) => "a menu item",
            ParamKind::String => "a string",
        }
    }
}

/// One parameter of an operator type, built by `#[derive(Params)]`.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub doc: &'static str,
    pub kind: ParamKind,
    pub default: Value,
    /// Hard limits: values are clamped to them.
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// The range a slider shows by default. Values may go past it.
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
}

impl ParamSpec {
    /// Matches the default's variant to the kind, for defaults written as literals (an integer for a float
    /// parameter, a string for a menu). Used by `#[derive(Params)]`.
    pub fn normalized(mut self) -> Self {
        self.default = match (self.kind, self.default) {
            (ParamKind::Float, Value::Int(v)) => Value::Float(v as f64),
            (ParamKind::Int, Value::Float(v)) if v.fract() == 0.0 => Value::Int(v as i64),
            (ParamKind::Menu(_), Value::String(v)) => Value::Menu(v),
            (_, default) => default,
        };
        self
    }

    /// Checks `value` against this parameter's type, turning integers into floats (and whole floats into integers)
    /// and clamping numbers to `min` and `max`.
    pub fn coerce(&self, value: Value) -> Result<Value, String> {
        let value = match (self.kind, value) {
            (ParamKind::Float, Value::Float(v)) => Value::Float(v),
            (ParamKind::Float, Value::Int(v)) => Value::Float(v as f64),
            (ParamKind::Int, Value::Int(v)) => Value::Int(v),
            (ParamKind::Int, Value::Float(v)) if v.fract() == 0.0 => Value::Int(v as i64),
            (ParamKind::Toggle, Value::Bool(v)) => Value::Bool(v),
            (ParamKind::String, Value::String(v)) => Value::String(v),
            (ParamKind::Menu(items), Value::Menu(v) | Value::String(v)) => {
                if !items.iter().any(|item| item.name == v) {
                    let names: Vec<_> = items.iter().map(|item| item.name).collect();
                    return Err(format!("{v:?} is not one of {}", names.join(", ")));
                }
                Value::Menu(v)
            }
            (kind, value) => return Err(format!("expected {}, got {}", kind.describe(), value.describe())),
        };
        Ok(match value {
            Value::Float(v) => Value::Float(self.clamp(v)),
            Value::Int(v) => Value::Int(self.clamp(v as f64) as i64),
            value => value,
        })
    }

    fn clamp(&self, v: f64) -> f64 {
        let v = self.min.map_or(v, |min| v.max(min));
        self.max.map_or(v, |max| v.min(max))
    }
}

/// A struct of parameters. Derive it with `#[derive(Params)]`.
pub trait Params: Sized {
    fn specs() -> Vec<ParamSpec>;
    /// Builds the struct from values already checked against `specs`, in the same order.
    fn from_values(values: &[Value]) -> Self;
}

impl Params for () {
    fn specs() -> Vec<ParamSpec> {
        Vec::new()
    }

    fn from_values(_: &[Value]) -> Self {}
}

/// A Rust type usable as a parameter field.
pub trait ParamType: Sized {
    fn kind() -> ParamKind;
    /// The default when the field has no `#[param(default = ...)]`.
    fn zero() -> Value;
    fn to_value(&self) -> Value;
    fn from_value(value: &Value) -> Self;
}

macro_rules! float_param {
    ($($ty:ty),*) => {$(
        impl ParamType for $ty {
            fn kind() -> ParamKind {
                ParamKind::Float
            }

            fn zero() -> Value {
                Value::Float(0.0)
            }

            fn to_value(&self) -> Value {
                Value::Float(*self as f64)
            }

            fn from_value(value: &Value) -> Self {
                match value {
                    Value::Float(v) => *v as $ty,
                    Value::Int(v) => *v as $ty,
                    _ => 0.0,
                }
            }
        }
    )*};
}

macro_rules! int_param {
    ($($ty:ty),*) => {$(
        impl ParamType for $ty {
            fn kind() -> ParamKind {
                ParamKind::Int
            }

            fn zero() -> Value {
                Value::Int(0)
            }

            fn to_value(&self) -> Value {
                Value::Int(*self as i64)
            }

            fn from_value(value: &Value) -> Self {
                match value {
                    Value::Int(v) => *v as $ty,
                    Value::Float(v) => *v as $ty,
                    _ => 0,
                }
            }
        }
    )*};
}

float_param!(f32, f64);
int_param!(i32, i64);

impl ParamType for bool {
    fn kind() -> ParamKind {
        ParamKind::Toggle
    }

    fn zero() -> Value {
        Value::Bool(false)
    }

    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }

    fn from_value(value: &Value) -> Self {
        matches!(value, Value::Bool(true))
    }
}

impl ParamType for String {
    fn kind() -> ParamKind {
        ParamKind::String
    }

    fn zero() -> Value {
        Value::String(String::new())
    }

    fn to_value(&self) -> Value {
        Value::String(self.clone())
    }

    fn from_value(value: &Value) -> Self {
        match value {
            Value::String(v) => v.clone(),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Menu, Params};

    #[derive(Menu, Clone, Copy, Debug, PartialEq)]
    enum Blend {
        Over,
        SoftLight,
        #[menu(name = "add", label = "Additive")]
        Plus,
    }

    /// Unused docs on the struct.
    #[derive(Params, Debug, PartialEq)]
    struct Example {
        /// Size of the features.
        /// In UV units.
        #[param(default = 1.5, min = 0.0, soft_max = 10)]
        period: f32,
        #[param(default = 4, min = 1, max = 8)]
        octave_count: i32,
        enabled: bool,
        #[param(default = "chan1", label = "Channel")]
        name: String,
        #[param(default = Blend::SoftLight)]
        blend: Blend,
    }

    #[test]
    fn derive_builds_the_schema() {
        let specs = Example::specs();
        let names: Vec<_> = specs.iter().map(|s| s.name).collect();
        assert_eq!(names, ["period", "octave_count", "enabled", "name", "blend"]);

        assert_eq!(specs[0].label, "Period");
        assert_eq!(specs[0].doc, "Size of the features.\nIn UV units.");
        assert_eq!(specs[0].default, Value::Float(1.5));
        assert_eq!((specs[0].min, specs[0].soft_max), (Some(0.0), Some(10.0)));
        assert_eq!(specs[1].label, "Octave Count");
        assert_eq!(specs[1].default, Value::Int(4));
        assert_eq!(specs[2].default, Value::Bool(false));
        assert_eq!(specs[3].label, "Channel");
        assert_eq!(specs[3].default, Value::String("chan1".into()));
        assert_eq!(specs[4].default, Value::Menu("soft_light".into()));

        let ParamKind::Menu(items) = specs[4].kind else {
            panic!("blend is a menu")
        };
        let items: Vec<_> = items.iter().map(|i| (i.name, i.label)).collect();
        assert_eq!(
            items,
            [("over", "Over"), ("soft_light", "Soft Light"), ("add", "Additive")]
        );
    }

    #[test]
    fn derive_reads_values() {
        let values: Vec<Value> = Example::specs().into_iter().map(|s| s.default).collect();
        assert_eq!(
            Example::from_values(&values),
            Example {
                period: 1.5,
                octave_count: 4,
                enabled: false,
                name: "chan1".into(),
                blend: Blend::SoftLight
            }
        );
    }

    #[test]
    fn coerce_checks_types_and_clamps() {
        let specs = Example::specs();
        assert_eq!(specs[0].coerce(Value::Int(2)), Ok(Value::Float(2.0)));
        assert_eq!(specs[0].coerce(Value::Float(-1.0)), Ok(Value::Float(0.0)));
        assert_eq!(specs[1].coerce(Value::Int(20)), Ok(Value::Int(8)));
        assert!(specs[1].coerce(Value::Float(2.5)).is_err());
        assert!(specs[2].coerce(Value::Int(1)).is_err());
        assert_eq!(
            specs[4].coerce(Value::String("add".into())),
            Ok(Value::Menu("add".into()))
        );
        assert!(specs[4].coerce(Value::Menu("plus".into())).is_err());
    }
}
