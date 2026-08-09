//! The Dart type emitter: the wire's resolved definitions, as Dart classes
//! whose encoded shape provably matches what serde produces.
//!
//! This is what makes compact MessagePack safe from a Dart client. Under the
//! compact codec a struct is an array and field order is the whole contract; a
//! hand-written model reads that order from memory, and these classes read it
//! from the Rust definitions at build time instead. Every generated type
//! carries `toWire({bool named})`, emitting compact arrays or named maps to
//! match the connection's codec, and `fromWire`, which accepts either shape.
//!
//! **The contract is deliberately narrow and loud.** Serde-derived structs and
//! enums; unit, newtype, tuple and struct variants; `Option`, `Vec`, maps,
//! sets, `Box`, tuples, `Duration` (as the generated `WireDuration`), `Uuid`
//! as a string with its caveat documented on [`Wire::dart_types`]; generics
//! monomorphised per instantiation, plaza's vocabulary included via the
//! embedded sources. Any serde attribute other than `bound`, any unsupported
//! type form, and any unresolvable name **fails the build naming the spot**,
//! because a generator that guesses produces exactly the silent wrong-order
//! corruption it exists to kill.
//!
//! [`Wire::dart_types`]: super::Wire::dart_types

use std::collections::BTreeMap;

use super::resolve::{bundle_hint, vocab_index, Wire};

pub(crate) fn generate(wire: &Wire) -> String {
  let (mut index, roots) = wire.scanned();
  for (name, def) in vocab_index() {
    index.entry(name).or_insert(def);
  }
  let items: BTreeMap<String, (syn::Item, String)> = index
    .into_iter()
    .map(|(name, def)| (name, (def.item, def.file.display().to_string())))
    .collect();

  let mut generator = Generator {
    items,
    plans: BTreeMap::new(),
    errors: Vec::new(),
  };
  for root in &roots {
    generator.instantiate(root, &[]);
  }
  if !generator.errors.is_empty() {
    panic!(
      "plaza-wire: cannot generate Dart types:\n  - {}",
      generator.errors.join("\n  - ")
    );
  }
  generator.render()
}

/// A resolved wire type, past every alias, substitution and container.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ty {
  Int,
  Double,
  Bool,
  Str,
  DurationTy,
  Option(Box<Ty>),
  List(Box<Ty>),
  MapTy(Box<Ty>, Box<Ty>),
  Tuple(Vec<Ty>),
  User(String),
}

enum Plan {
  UnitEnum { variants: Vec<String> },
  DataEnum { variants: Vec<VariantPlan> },
  Struct { fields: Vec<Field> },
  NewType { inner: Ty },
  TupleStruct { elems: Vec<Ty> },
}

enum VariantPlan {
  Unit(String),
  NewType(String, Ty),
  Tuple(String, Vec<Ty>),
  Struct(String, Vec<Field>),
}

struct Field {
  rust: String,
  dart: String,
  ty: Ty,
}

struct Generator {
  items: BTreeMap<String, (syn::Item, String)>,
  plans: BTreeMap<String, Plan>,
  errors: Vec<String>,
}

impl Generator {
  /// Ensures the instantiation `base<args>` has a plan, returning its Dart name.
  fn instantiate(&mut self, base: &str, args: &[Ty]) -> String {
    let dart = mangle(base, args);
    if self.plans.contains_key(&dart) {
      return dart;
    }
    // Reserve before descending, so a recursive type terminates.
    self.plans.insert(dart.clone(), Plan::Struct { fields: Vec::new() });

    let Some((item, file)) = self.items.get(base).cloned() else {
      self.errors.push(format!("no definition for `{base}`"));
      return dart;
    };
    let plan = match &item {
      syn::Item::Struct(s) => {
        self.check_serde_attrs(&s.attrs, base, &file);
        let subst = self.substitution(&s.generics, args, base, &file);
        match &s.fields {
          syn::Fields::Named(named) => Plan::Struct {
            fields: self.fields(named.named.iter(), &subst, base, &file),
          },
          syn::Fields::Unnamed(unnamed) if unnamed.unnamed.len() == 1 => Plan::NewType {
            inner: self.lower(&unnamed.unnamed[0].ty, &subst, base, &file),
          },
          syn::Fields::Unnamed(unnamed) => Plan::TupleStruct {
            elems: unnamed.unnamed.iter().map(|f| self.lower(&f.ty, &subst, base, &file)).collect(),
          },
          syn::Fields::Unit => Plan::TupleStruct { elems: Vec::new() },
        }
      }
      syn::Item::Enum(e) => {
        self.check_serde_attrs(&e.attrs, base, &file);
        let subst = self.substitution(&e.generics, args, base, &file);
        let mut unit_only = true;
        let mut variants = Vec::new();
        for variant in &e.variants {
          self.check_serde_attrs(&variant.attrs, base, &file);
          let name = variant.ident.to_string();
          let plan = match &variant.fields {
            syn::Fields::Unit => VariantPlan::Unit(name),
            syn::Fields::Unnamed(unnamed) if unnamed.unnamed.len() == 1 => {
              unit_only = false;
              VariantPlan::NewType(name, self.lower(&unnamed.unnamed[0].ty, &subst, base, &file))
            }
            syn::Fields::Unnamed(unnamed) => {
              unit_only = false;
              VariantPlan::Tuple(name, unnamed.unnamed.iter().map(|f| self.lower(&f.ty, &subst, base, &file)).collect())
            }
            syn::Fields::Named(named) => {
              unit_only = false;
              VariantPlan::Struct(name, self.fields(named.named.iter(), &subst, base, &file))
            }
          };
          variants.push(plan);
        }
        if unit_only {
          Plan::UnitEnum {
            variants: variants
              .into_iter()
              .map(|v| match v {
                VariantPlan::Unit(name) => name,
                _ => unreachable!(),
              })
              .collect(),
          }
        } else {
          Plan::DataEnum { variants }
        }
      }
      other => {
        self.errors.push(format!(
          "`{base}` ({file}) is not a struct or enum the generator can emit: {}",
          item_kind(other)
        ));
        return dart;
      }
    };
    self.plans.insert(dart.clone(), plan);
    dart
  }

  fn fields<'a>(
    &mut self,
    fields: impl Iterator<Item = &'a syn::Field>,
    subst: &BTreeMap<String, Ty>,
    base: &str,
    file: &str,
  ) -> Vec<Field> {
    fields
      .map(|field| {
        self.check_serde_attrs(&field.attrs, base, file);
        let rust = field.ident.as_ref().expect("named field").to_string();
        Field {
          dart: camel(&rust),
          ty: self.lower(&field.ty, subst, base, file),
          rust,
        }
      })
      .collect()
  }

  fn substitution(&mut self, generics: &syn::Generics, args: &[Ty], base: &str, file: &str) -> BTreeMap<String, Ty> {
    let params: Vec<String> = generics.type_params().map(|p| p.ident.to_string()).collect();
    if params.len() != args.len() {
      self.errors.push(format!(
        "`{base}` ({file}) takes {} type parameters, instantiated with {}",
        params.len(),
        args.len()
      ));
      return BTreeMap::new();
    }
    params.into_iter().zip(args.iter().cloned()).collect()
  }

  /// Any serde attribute except `bound` changes the wire in ways this
  /// generator does not model, so it is refused rather than mis-encoded.
  fn check_serde_attrs(&mut self, attrs: &[syn::Attribute], base: &str, file: &str) {
    for attr in attrs {
      if attr.path().is_ident("serde") {
        let tokens = attr.meta.require_list().map(|l| l.tokens.to_string()).unwrap_or_default();
        if !tokens.trim_start().starts_with("bound") {
          self.errors.push(format!(
            "`{base}` ({file}) carries `#[serde({tokens})]`, which the Dart generator does not model; only \
             `bound` is supported"
          ));
        }
      }
    }
  }

  fn lower(&mut self, ty: &syn::Type, subst: &BTreeMap<String, Ty>, base: &str, file: &str) -> Ty {
    match ty {
      syn::Type::Path(path) => {
        let Some(segment) = path.path.segments.last() else {
          self.errors.push(format!("`{base}` ({file}): empty type path"));
          return Ty::Int;
        };
        let name = segment.ident.to_string();
        if let Some(substituted) = subst.get(&name) {
          return substituted.clone();
        }
        let args: Vec<Ty> = match &segment.arguments {
          syn::PathArguments::AngleBracketed(list) => list
            .args
            .iter()
            .filter_map(|arg| match arg {
              syn::GenericArgument::Type(inner) => Some(self.lower(inner, subst, base, file)),
              _ => None,
            })
            .collect(),
          _ => Vec::new(),
        };
        match name.as_str() {
          "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize" => Ty::Int,
          "f32" | "f64" => Ty::Double,
          "bool" => Ty::Bool,
          "String" | "str" | "Uuid" => Ty::Str,
          "Duration" => Ty::DurationTy,
          "Option" => Ty::Option(Box::new(self.one_arg(args, "Option", base, file))),
          "Vec" | "HashSet" | "BTreeSet" | "VecDeque" => Ty::List(Box::new(self.one_arg(args, &name, base, file))),
          "Box" | "Arc" | "Rc" => self.one_arg(args, &name, base, file),
          "HashMap" | "BTreeMap" => {
            if args.len() != 2 {
              self.errors.push(format!("`{base}` ({file}): {name} needs two type arguments"));
              return Ty::Int;
            }
            let mut iter = args.into_iter();
            Ty::MapTy(Box::new(iter.next().unwrap()), Box::new(iter.next().unwrap()))
          }
          _ => match self.items.get(&name) {
            Some((syn::Item::Type(alias), alias_file)) => {
              if !args.is_empty() {
                self.errors.push(format!("`{base}` ({file}): generic alias `{name}` is not supported"));
                return Ty::Int;
              }
              let (alias, alias_file) = (alias.clone(), alias_file.clone());
              self.lower(&alias.ty, &BTreeMap::new(), &name, &alias_file)
            }
            Some(_) => Ty::User(self.instantiate(&name, &args)),
            None => {
              match bundle_hint(&name) {
                Some(hint) => self.errors.push(format!(
                  "`{base}` ({file}) references `{name}`, plaza vocabulary that is not included; add {hint}"
                )),
                None => self.errors.push(format!(
                  "`{base}` ({file}) references `{name}`, which the generator cannot place; define it in a scanned \
                   directory, add .also_scan(dir), or include a vendored copy with .vocab(&[(label, source)])"
                )),
              }
              Ty::Int
            }
          },
        }
      }
      syn::Type::Tuple(tuple) => Ty::Tuple(tuple.elems.iter().map(|e| self.lower(e, subst, base, file)).collect()),
      syn::Type::Paren(paren) => self.lower(&paren.elem, subst, base, file),
      syn::Type::Group(group) => self.lower(&group.elem, subst, base, file),
      syn::Type::Reference(reference) => self.lower(&reference.elem, subst, base, file),
      other => {
        let kind = match other {
          syn::Type::TraitObject(_) => "a trait object",
          syn::Type::ImplTrait(_) => "an impl-trait type",
          syn::Type::Macro(_) => "a macro-generated type",
          syn::Type::BareFn(_) => "a function pointer",
          syn::Type::Array(_) | syn::Type::Slice(_) => "an array or slice (use Vec)",
          _ => "an unsupported type form",
        };
        self.errors.push(format!("`{base}` ({file}) uses {kind}, which the generator does not model"));
        Ty::Int
      }
    }
  }

  fn one_arg(&mut self, mut args: Vec<Ty>, container: &str, base: &str, file: &str) -> Ty {
    if args.len() != 1 {
      self.errors.push(format!("`{base}` ({file}): {container} needs one type argument"));
      return Ty::Int;
    }
    args.remove(0)
  }

  fn render(&self) -> String {
    let mut out = String::from(
      "// Generated by plaza_wire::build::Wire::dart_types from the server's wire\n\
       // definitions. Do not edit; the server's build script rewrites it when the\n\
       // wire changes.\n\
       //\n\
       // Every type encodes with `toWire(named: ...)`: compact arrays for plaza's\n\
       // MsgPackCodec, named maps for JSON and MsgPackNamedCodec. `fromWire`\n\
       // accepts either shape.\n\
       // ignore_for_file: unused_element\n\n\
       /// Rust's `std::time::Duration` on the wire: seconds and nanoseconds.\n\
       class WireDuration {\n\
       \x20\x20const WireDuration({required this.secs, required this.nanos});\n\
       \x20\x20final int secs;\n\
       \x20\x20final int nanos;\n\
       \x20\x20Object? toWire({bool named = false}) =>\n\
       \x20\x20\x20\x20\x20\x20named ? <String, Object?>{'secs': secs, 'nanos': nanos} : <Object?>[secs, nanos];\n\
       \x20\x20static WireDuration fromWire(Object? wire) {\n\
       \x20\x20\x20\x20if (wire is Map) return WireDuration(secs: wire['secs'] as int, nanos: wire['nanos'] as int);\n\
       \x20\x20\x20\x20final l = wire as List<Object?>;\n\
       \x20\x20\x20\x20return WireDuration(secs: l[0] as int, nanos: l[1] as int);\n\
       \x20\x20}\n\
       }\n",
    );
    for (dart, plan) in &self.plans {
      out.push('\n');
      out.push_str(&render_plan(dart, plan));
    }
    out
  }
}

fn render_plan(dart: &str, plan: &Plan) -> String {
  match plan {
    Plan::UnitEnum { variants } => {
      let members = variants
        .iter()
        .map(|v| format!("  {}('{v}')", camel(&snake(v))))
        .collect::<Vec<_>>()
        .join(",\n");
      format!(
        "enum {dart} {{\n{members};\n\n  const {dart}(this.wireName);\n  final String wireName;\n\n  Object? \
         toWire({{bool named = false}}) => wireName;\n\n  static {dart} fromWire(Object? wire) =>\n      \
         values.firstWhere((v) => v.wireName == wire as String,\n          orElse: () => throw \
         FormatException('unknown {dart} variant: $wire'));\n}}\n"
      )
    }
    Plan::NewType { inner } => {
      let ty = dart_type(inner);
      format!(
        "class {dart} {{\n  const {dart}(this.value);\n  final {ty} value;\n\n  Object? toWire({{bool named = \
         false}}) {{\n    final v = value;\n    return {};\n  }}\n\n  static {dart} fromWire(Object? wire) => \
         {dart}({});\n}}\n",
        encode("v", inner),
        decode("wire", inner)
      )
    }
    Plan::TupleStruct { elems } => {
      let fields: Vec<String> = (0..elems.len()).map(|i| format!("f{i}")).collect();
      let decls = elems
        .iter()
        .zip(&fields)
        .map(|(ty, f)| format!("  final {} {f};", dart_type(ty)))
        .collect::<Vec<_>>()
        .join("\n");
      let params = fields.iter().map(|f| format!("this.{f}")).collect::<Vec<_>>().join(", ");
      let encodes = elems
        .iter()
        .zip(&fields)
        .map(|(ty, f)| encode(f, ty))
        .collect::<Vec<_>>()
        .join(", ");
      let decodes = elems
        .iter()
        .enumerate()
        .map(|(i, ty)| decode(&format!("l[{i}]"), ty))
        .collect::<Vec<_>>()
        .join(", ");
      format!(
        "class {dart} {{\n  const {dart}({params});\n{decls}\n\n  Object? toWire({{bool named = false}}) => \
         <Object?>[{encodes}];\n\n  static {dart} fromWire(Object? wire) {{\n    final l = wire as \
         List<Object?>;\n    return {dart}({decodes});\n  }}\n}}\n"
      )
    }
    Plan::Struct { fields } => render_struct(dart, fields),
    Plan::DataEnum { variants } => render_data_enum(dart, variants),
  }
}

fn render_struct(dart: &str, fields: &[Field]) -> String {
  let decls = fields
    .iter()
    .map(|f| format!("  final {} {};", dart_type(&f.ty), f.dart))
    .collect::<Vec<_>>()
    .join("\n");
  let params = fields
    .iter()
    .map(|f| format!("required this.{}", f.dart))
    .collect::<Vec<_>>()
    .join(", ");
  format!(
    "class {dart} {{\n  const {dart}({{{params}}});\n{decls}\n\n  Object? toWire({{bool named = false}}) \
     {{\n{}    return named\n        ? <String, Object?>{{{}}}\n        : <Object?>[{}];\n  }}\n\n  static {dart} \
     fromWire(Object? wire) {{\n    if (wire is Map) {{\n      return {dart}({});\n    }}\n    final l = wire as \
     List<Object?>;\n    return {dart}({});\n  }}\n}}\n",
    locals(fields),
    named_entries(fields),
    compact_entries(fields),
    map_args(fields),
    list_args(fields),
  )
}

fn render_data_enum(dart: &str, variants: &[VariantPlan]) -> String {
  let mut subclasses = String::new();
  let mut unit_cases = String::new();
  let mut body_cases = String::new();

  for variant in variants {
    match variant {
      VariantPlan::Unit(name) => {
        let class = format!("{dart}{name}");
        subclasses.push_str(&format!(
          "\nclass {class} extends {dart} {{\n  const {class}();\n  @override\n  Object? toWire({{bool named = \
           false}}) => '{name}';\n}}\n"
        ));
        unit_cases.push_str(&format!("      case '{name}':\n        return const {class}();\n"));
      }
      VariantPlan::NewType(name, ty) => {
        let class = format!("{dart}{name}");
        subclasses.push_str(&format!(
          "\nclass {class} extends {dart} {{\n  const {class}(this.value);\n  final {} value;\n  @override\n  \
           Object? toWire({{bool named = false}}) {{\n    final v = value;\n    return <String, \
           Object?>{{'{name}': {}}};\n  }}\n}}\n",
          dart_type(ty),
          encode("v", ty)
        ));
        body_cases.push_str(&format!(
          "      case '{name}':\n        return {class}({});\n",
          decode("body", ty)
        ));
      }
      VariantPlan::Tuple(name, elems) => {
        let class = format!("{dart}{name}");
        let fields: Vec<String> = (0..elems.len()).map(|i| format!("f{i}")).collect();
        let decls = elems
          .iter()
          .zip(&fields)
          .map(|(ty, f)| format!("  final {} {f};", dart_type(ty)))
          .collect::<Vec<_>>()
          .join("\n");
        let params = fields.iter().map(|f| format!("this.{f}")).collect::<Vec<_>>().join(", ");
        let encodes = elems.iter().zip(&fields).map(|(ty, f)| encode(f, ty)).collect::<Vec<_>>().join(", ");
        let decodes = elems
          .iter()
          .enumerate()
          .map(|(i, ty)| decode(&format!("bl[{i}]"), ty))
          .collect::<Vec<_>>()
          .join(", ");
        subclasses.push_str(&format!(
          "\nclass {class} extends {dart} {{\n  const {class}({params});\n{decls}\n  @override\n  Object? \
           toWire({{bool named = false}}) => <String, Object?>{{'{name}': <Object?>[{encodes}]}};\n}}\n"
        ));
        body_cases.push_str(&format!(
          "      case '{name}': {{\n        final bl = body as List<Object?>;\n        return \
           {class}({decodes});\n      }}\n"
        ));
      }
      VariantPlan::Struct(name, fields) => {
        let class = format!("{dart}{name}");
        let decls = fields
          .iter()
          .map(|f| format!("  final {} {};", dart_type(&f.ty), f.dart))
          .collect::<Vec<_>>()
          .join("\n");
        let params = fields
          .iter()
          .map(|f| format!("required this.{}", f.dart))
          .collect::<Vec<_>>()
          .join(", ");
        subclasses.push_str(&format!(
          "\nclass {class} extends {dart} {{\n  const {class}({{{params}}});\n{decls}\n  @override\n  Object? \
           toWire({{bool named = false}}) {{\n{}    return <String, Object?>{{\n      '{name}': named\n          ? \
           <String, Object?>{{{}}}\n          : <Object?>[{}],\n    }};\n  }}\n}}\n",
          locals(fields),
          named_entries(fields),
          compact_entries(fields),
        ));
        body_cases.push_str(&format!(
          "      case '{name}': {{\n        if (body is Map) {{\n          return {class}({});\n        }}\n        \
           final bl = body as List<Object?>;\n        return {class}({});\n      }}\n",
          map_args_from(fields, "body"),
          list_args_from(fields, "bl"),
        ));
      }
    }
  }

  format!(
    "sealed class {dart} {{\n  const {dart}();\n  Object? toWire({{bool named = false}});\n\n  static {dart} \
     fromWire(Object? wire) {{\n    if (wire is String) {{\n      switch (wire) {{\n{unit_cases}      }}\n      \
     throw FormatException('unknown {dart} variant: $wire');\n    }}\n    final m = wire as Map;\n    final name = \
     m.keys.first as String;\n    final body = m[name];\n    switch (name) {{\n{body_cases}    }}\n    throw \
     FormatException('unknown {dart} variant: $name');\n  }}\n}}\n{subclasses}"
  )
}

fn locals(fields: &[Field]) -> String {
  fields
    .iter()
    .map(|f| format!("    final {}0 = {};\n", f.dart, f.dart))
    .collect()
}

fn named_entries(fields: &[Field]) -> String {
  fields
    .iter()
    .map(|f| format!("'{}': {}", f.rust, encode(&format!("{}0", f.dart), &f.ty)))
    .collect::<Vec<_>>()
    .join(", ")
}

fn compact_entries(fields: &[Field]) -> String {
  fields
    .iter()
    .map(|f| encode(&format!("{}0", f.dart), &f.ty))
    .collect::<Vec<_>>()
    .join(", ")
}

fn map_args(fields: &[Field]) -> String {
  map_args_from(fields, "wire")
}

fn map_args_from(fields: &[Field], map: &str) -> String {
  fields
    .iter()
    .map(|f| format!("{}: {}", f.dart, decode(&format!("{map}['{}']", f.rust), &f.ty)))
    .collect::<Vec<_>>()
    .join(", ")
}

fn list_args(fields: &[Field]) -> String {
  list_args_from(fields, "l")
}

fn list_args_from(fields: &[Field], list: &str) -> String {
  fields
    .iter()
    .enumerate()
    .map(|(i, f)| format!("{}: {}", f.dart, decode(&format!("{list}[{i}]"), &f.ty)))
    .collect::<Vec<_>>()
    .join(", ")
}

fn dart_type(ty: &Ty) -> String {
  match ty {
    Ty::Int => "int".into(),
    Ty::Double => "double".into(),
    Ty::Bool => "bool".into(),
    Ty::Str => "String".into(),
    Ty::DurationTy => "WireDuration".into(),
    Ty::Option(inner) => format!("{}?", dart_type(inner)),
    Ty::List(inner) => format!("List<{}>", dart_type(inner)),
    Ty::MapTy(k, v) => format!("Map<{}, {}>", dart_type(k), dart_type(v)),
    Ty::Tuple(elems) => format!("({})", elems.iter().map(dart_type).collect::<Vec<_>>().join(", ")),
    Ty::User(name) => name.clone(),
  }
}

/// An expression encoding `expr` (already a promotable local) as its wire form.
fn encode(expr: &str, ty: &Ty) -> String {
  match ty {
    Ty::Int | Ty::Double | Ty::Bool | Ty::Str => expr.into(),
    Ty::DurationTy | Ty::User(_) => format!("{expr}.toWire(named: named)"),
    Ty::Option(inner) => format!("{expr} == null ? null : {}", encode(expr, inner)),
    Ty::List(inner) => format!("[for (final e in {expr}) {}]", encode("e", inner)),
    Ty::MapTy(k, v) => format!(
      "{{for (final e in {expr}.entries) {}: {}}}",
      encode("e.key", k),
      encode("e.value", v)
    ),
    Ty::Tuple(elems) => format!(
      "<Object?>[{}]",
      elems
        .iter()
        .enumerate()
        .map(|(i, e)| encode(&format!("{expr}.${}", i + 1), e))
        .collect::<Vec<_>>()
        .join(", ")
    ),
  }
}

/// An expression decoding the untyped `expr` into the field's Dart type.
fn decode(expr: &str, ty: &Ty) -> String {
  match ty {
    Ty::Int => format!("{expr} as int"),
    Ty::Double => format!("({expr} as num).toDouble()"),
    Ty::Bool => format!("{expr} as bool"),
    Ty::Str => format!("{expr} as String"),
    Ty::DurationTy => format!("WireDuration.fromWire({expr})"),
    Ty::User(name) => format!("{name}.fromWire({expr})"),
    Ty::Option(inner) => format!("{expr} == null ? null : {}", decode(expr, inner)),
    Ty::List(inner) => format!("[for (final e in ({expr} as List<Object?>)) {}]", decode("e", inner)),
    Ty::MapTy(k, v) => format!(
      "{{for (final e in ({expr} as Map<Object?, Object?>).entries) {}: {}}}",
      decode("e.key", k),
      decode("e.value", v)
    ),
    Ty::Tuple(elems) => format!(
      "(() {{ final t = {expr} as List<Object?>; return ({}); }})()",
      elems
        .iter()
        .enumerate()
        .map(|(i, e)| decode(&format!("t[{i}]"), e))
        .collect::<Vec<_>>()
        .join(", ")
    ),
  }
}

fn mangle(base: &str, args: &[Ty]) -> String {
  let mut out = base.to_string();
  for arg in args {
    out.push_str(&arg_name(arg));
  }
  out
}

fn arg_name(ty: &Ty) -> String {
  match ty {
    Ty::Int => "Int".into(),
    Ty::Double => "Double".into(),
    Ty::Bool => "Bool".into(),
    Ty::Str => "String".into(),
    Ty::DurationTy => "Duration".into(),
    Ty::Option(inner) => format!("Opt{}", arg_name(inner)),
    Ty::List(inner) => format!("ListOf{}", arg_name(inner)),
    Ty::MapTy(k, v) => format!("MapOf{}{}", arg_name(k), arg_name(v)),
    Ty::Tuple(elems) => format!("Tuple{}", elems.iter().map(arg_name).collect::<String>()),
    Ty::User(name) => name.clone(),
  }
}

fn camel(snake_name: &str) -> String {
  let mut out = String::new();
  let mut upper = false;
  for c in snake_name.chars() {
    if c == '_' {
      upper = true;
    } else if upper {
      out.extend(c.to_uppercase());
      upper = false;
    } else {
      out.push(c);
    }
  }
  out
}

fn snake(pascal: &str) -> String {
  let mut out = String::new();
  for (i, c) in pascal.chars().enumerate() {
    if c.is_uppercase() {
      if i > 0 {
        out.push('_');
      }
      out.extend(c.to_lowercase());
    } else {
      out.push(c);
    }
  }
  out
}

fn item_kind(item: &syn::Item) -> &'static str {
  match item {
    syn::Item::Type(_) => "a type alias cannot be a root",
    syn::Item::Union(_) => "unions do not serialize",
    _ => "an unsupported item kind",
  }
}
