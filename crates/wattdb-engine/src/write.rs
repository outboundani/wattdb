//! Single-item operations: get, put, create, delete, remove, update, patch,
//! upsert and transaction condition checks (ElectroDB's chain state).

use indexmap::IndexMap;

use crate::error::{Error, FieldError, IncompleteCompositesError, Result, ValidationError};
use crate::expr::{FilterExpr, UpdateExpr, format_expression_name};
use crate::item::{
    FormatOptions, Method as Mutation, check_create, format_item, fulfill, get_validate, invalid,
    owns_item, put_item_from_record, root_names,
};
use crate::keys::{
    ImpactOptions, KeyOptions, expect_index_facets, index_keys, merge_attrs, primary_key,
    put_keys_deleted,
};
use crate::model::{IndexType, KeyType, Model};
use crate::ops::{OpTarget, Path, apply_op, resolve};
use crate::request::{Operation, Params};
use crate::value::{Attrs, Value};

/// A single-item operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Put,
    Create,
    Delete,
    Remove,
    Update,
    Patch,
    Upsert,
    Check,
}

/// One attribute path and value, as in ElectroDB's `set({...})` etc.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: String,
    pub value: Value,
}

impl Entry {
    pub fn new(path: impl Into<String>, value: Value) -> Self {
        Entry {
            path: path.into(),
            value,
        }
    }
}

/// One operation of a `data()` callback.
#[derive(Clone, Debug)]
pub struct DataOp {
    /// set, remove, add, subtract, append, delete, ifNotExists, ...
    pub op: String,
    pub path: Path,
    pub values: Vec<Value>,
}

/// A condition tree for `where()`.
#[derive(Clone, Debug)]
pub enum Cond {
    /// One ElectroDB where operation: eq, ne, gt, gte, lt, lte, between,
    /// begins, exists, notExists, contains, notContains, size, type.
    Op {
        op: String,
        path: Path,
        values: Vec<Value>,
    },
    /// Rendered `(a) AND (b)`.
    And(Vec<Cond>),
    /// Rendered `(a) OR (b)`.
    Or(Vec<Cond>),
    /// Rendered `NOT (a)`.
    Not(Box<Cond>),
    /// A raw expression with its own placeholders.
    Raw {
        expression: String,
        names: IndexMap<String, String>,
        values: Attrs,
    },
}

/// The execution options that shape a request.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Overrides the table.
    pub table: String,
    /// `""`, none, all_old, updated_old, all_new or updated_new.
    pub response: String,
    pub consistent: bool,
    /// `""`, `"true"` or `"all_old"` (returnOnConditionCheckFailure).
    pub return_rejected: String,
    pub attributes: Vec<String>,
    pub include_keys: bool,
    /// True when the request will be sent (`.go()`), not just rendered
    /// (`.params()`); it changes the get projection.
    pub exec: bool,
    /// Set for transaction items: `response` then sets
    /// ReturnValuesOnConditionCheckFailure.
    pub transaction: bool,
}

#[derive(Clone)]
struct UpsertEntry {
    op: String,
    name: String,
    value: Value,
}

/// Builds one single-item operation. Errors are kept and returned from
/// `build` (decision D49).
#[derive(Clone)]
pub struct Write<'m> {
    m: &'m Model,
    method: Method,
    pk: Attrs,
    sk: Attrs,
    record: Option<Attrs>,
    upd: UpdateExpr,
    cond: FilterExpr,
    upsert: Vec<UpsertEntry>,
    err: Option<Error>,
    /// The key composites ElectroDB echoes in some responses.
    include: Attrs,
}

/// Mirrors `_expectFacets`.
pub(crate) fn expect_facets(obj: &Attrs, names: &[String]) -> Result<Attrs> {
    let mut out = Attrs::new();
    let mut missing = Vec::new();
    for n in names {
        match obj.get(n) {
            Some(v) if !v.is_undefined() => {
                out.insert(n.clone(), v.clone());
            }
            _ => missing.push(n.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(IncompleteCompositesError {
            missing,
            ..Default::default()
        }
        .into());
    }
    Ok(out)
}

impl<'m> Write<'m> {
    fn new(m: &'m Model, method: Method) -> Self {
        Write {
            m,
            method,
            pk: Attrs::new(),
            sk: Attrs::new(),
            record: None,
            upd: UpdateExpr::new(),
            cond: FilterExpr::new(),
            upsert: Vec::new(),
            err: None,
            include: Attrs::new(),
        }
    }

    /// Wraps a condition state in a write, so conditions can be rendered
    /// against it (queries use this for filters).
    pub(crate) fn for_filter(m: &'m Model, cond: FilterExpr) -> Self {
        let mut w = Write::new(m, Method::Get);
        w.cond = cond;
        w
    }

    pub(crate) fn into_filter(self) -> FilterExpr {
        self.cond
    }

    fn fail(&mut self, err: Error) {
        if self.err.is_none() {
            self.err = Some(err);
        }
    }

    fn key_facets(&mut self, obj: &Attrs) {
        let ix = self.m.table_index();
        match expect_facets(obj, &ix.pk.composites) {
            Ok(pk) => self.pk = pk,
            Err(e) => return self.fail(e),
        }
        match expect_facets(obj, &ix.sk.composites) {
            Ok(sk) => self.sk = sk,
            Err(e) => return self.fail(e),
        }
        self.include = merge_attrs(&[&self.sk, &self.pk]);
    }

    /// Starts a get.
    pub fn get(m: &'m Model, key: &Attrs) -> Self {
        let mut w = Write::new(m, Method::Get);
        w.key_facets(key);
        w.include = Attrs::new();
        w
    }

    /// Starts a transaction condition check.
    pub fn check(m: &'m Model, key: &Attrs) -> Self {
        let mut w = Write::new(m, Method::Check);
        w.key_facets(key);
        w.include = Attrs::new();
        w
    }

    /// Starts a put (create when `create` is true).
    pub fn put(m: &'m Model, item: &Attrs, create: bool) -> Self {
        let mut w = Write::new(m, if create { Method::Create } else { Method::Put });
        let record = match check_create(m, item) {
            Ok(r) => r,
            Err(e) => {
                w.fail(e);
                return w;
            }
        };
        if create {
            w.exists_conditions("notExists");
        }
        w.key_facets(&record);
        w.record = Some(record);
        w.include = Attrs::new();
        w
    }

    /// Starts a delete (remove when `remove` is true).
    pub fn delete(m: &'m Model, key: &Attrs, remove: bool) -> Self {
        let mut w = Write::new(
            m,
            if remove {
                Method::Remove
            } else {
                Method::Delete
            },
        );
        if remove {
            w.exists_conditions("exists");
        }
        w.key_facets(key);
        w
    }

    /// Starts an update (patch when `patch` is true).
    pub fn update(m: &'m Model, key: &Attrs, patch: bool) -> Self {
        let mut w = Write::new(m, if patch { Method::Patch } else { Method::Update });
        if patch {
            w.exists_conditions("exists");
        }
        w.key_facets(key);
        w
    }

    /// Starts an upsert with its "set" data, in the given attribute order.
    pub fn upsert(m: &'m Model, item: &Attrs, order: &[String]) -> Self {
        let mut w = Write::new(m, Method::Upsert);
        w.add_upsert("set", item, order);
        w
    }

    pub fn method(&self) -> Method {
        self.method
    }

    fn exists_conditions(&mut self, op: &str) {
        let ix = self.m.table_index();
        self.cond.unsafe_set(false, op, &ix.pk.field, &[]);
        if ix.has_sk {
            self.cond.unsafe_set(false, op, &ix.sk.field, &[]);
        }
    }

    fn add_upsert(&mut self, op: &str, data: &Attrs, order: &[String]) {
        for name in order {
            let Some(v) = data.get(name) else { continue };
            let entry = UpsertEntry {
                op: op.into(),
                name: name.clone(),
                value: v.clone(),
            };
            let mut replaced = false;
            for e in self.upsert.iter_mut() {
                if e.name == *name {
                    *e = entry.clone();
                    replaced = true;
                }
            }
            if !replaced {
                self.upsert.push(entry);
            }
        }
    }

    /// Mirrors `Schema.checkUpdate`: validate each provided value, rejecting
    /// readOnly attributes unless allowed.
    fn check_update(&self, entries: &[Entry], allow_read_only: bool) -> Result<()> {
        for e in entries {
            let Some(a) = self.m.attribute_at_exact_path(&e.path) else {
                continue;
            };
            if a.read_only && !allow_read_only {
                return Err(invalid(
                    a,
                    format!("Attribute {:?} is Read-Only and cannot be updated", a.path),
                ));
            }
            get_validate(a, e.value.clone())?;
        }
        Ok(())
    }

    /// Mirrors `AttributeOperationProxy.performOperation`.
    fn perform_operation(
        &mut self,
        op: &str,
        path: &Path,
        value: Value,
        force: bool,
    ) -> Result<()> {
        if value.is_undefined() {
            return Ok(());
        }
        let r = resolve(self.m, path)?;
        let Some(target) = r.target else {
            return Ok(());
        };
        apply_op(self.m, OpTarget::Update(&mut self.upd), op, path, &[value])?;
        if target.read_only && !force {
            return Err(invalid(
                &target,
                format!(
                    "Attribute {:?} is Read-Only and cannot be updated",
                    target.path
                ),
            ));
        }
        Ok(())
    }

    /// Runs an update operation over entries: set, add, subtract, append,
    /// delete or ifNotExists (ElectroDB's chain methods of the same names).
    pub fn apply(&mut self, op: &str, entries: &[Entry]) -> &mut Self {
        if self.err.is_some() {
            return self;
        }
        if self.method == Method::Upsert {
            if let Err(e) = self.check_update(entries, true) {
                self.fail(e);
                return self;
            }
            let mut data = Attrs::new();
            let mut order = Vec::new();
            for e in entries {
                data.insert(e.path.clone(), e.value.clone());
                order.push(e.path.clone());
            }
            self.add_upsert(op, &data, &order);
            return self;
        }
        if let Err(e) = self.check_update(entries, false) {
            self.fail(e);
            return self;
        }
        for e in entries {
            if let Err(err) =
                self.perform_operation(op, &Path::parse(&e.path), e.value.clone(), false)
            {
                self.fail(err);
                return self;
            }
        }
        self
    }

    /// Mirrors the update `remove([...])` method.
    pub fn remove(&mut self, paths: &[String]) -> &mut Self {
        if self.err.is_some() {
            return self;
        }
        for p in paths {
            let err = match self.m.attribute_at_path(p) {
                None => Some(Error::from(ValidationError {
                    fields: vec![FieldError {
                        path: p.clone(),
                        reason: format!("Attribute {p:?} does not exist on model."),
                        index: None,
                        cause: None,
                    }],
                })),
                Some(a) if a.read_only => Some(invalid(
                    a,
                    format!("Attribute {:?} is Read-Only and cannot be removed", a.path),
                )),
                Some(a) if a.required => Some(invalid(
                    a,
                    format!("Attribute {:?} is Required and cannot be removed", a.path),
                )),
                Some(_) => None,
            };
            if let Some(e) = err {
                self.fail(e);
                return self;
            }
        }
        for p in paths {
            let path = Path::parse(p);
            let r = match resolve(self.m, &path) {
                Ok(r) => r,
                Err(e) => {
                    self.fail(e);
                    return self;
                }
            };
            if r.target.is_none() {
                continue;
            }
            if let Err(e) = apply_op(
                self.m,
                OpTarget::Update(&mut self.upd),
                "remove",
                &path,
                &[],
            ) {
                self.fail(e);
                return self;
            }
        }
        self
    }

    /// Mirrors `data()`: raw operations by path, each checked afterwards like
    /// ElectroDB's checkOperation.
    pub fn data(&mut self, ops: &[DataOp]) -> &mut Self {
        if self.err.is_some() {
            return self;
        }
        for op in ops {
            let r = match resolve(self.m, &op.path) {
                Ok(r) => r,
                Err(e) => {
                    self.fail(e);
                    return self;
                }
            };
            if r.target.is_none() {
                self.fail(Error::invalid(format!(
                    "invalid/unknown property passed to operation {:?}",
                    op.op
                )));
                return self;
            }
            if let Err(e) = apply_op(
                self.m,
                OpTarget::Update(&mut self.upd),
                &op.op,
                &op.path,
                &op.values,
            ) {
                self.fail(e);
                return self;
            }
        }
        let refs: Vec<(String, std::sync::Arc<crate::model::Attribute>)> = self
            .upd
            .state
            .refs
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (path, a) in refs {
            let operation = self
                .upd
                .state
                .impacted
                .get(&path)
                .cloned()
                .unwrap_or_default();
            let value = self
                .upd
                .state
                .paths
                .get(&path)
                .map(|p| p.value.clone())
                .unwrap_or_default();
            if a.required && operation == "remove" {
                self.fail(invalid(
                    &a,
                    format!("Attribute {:?} is Required and cannot be removed", a.path),
                ));
                return self;
            }
            if a.read_only {
                self.fail(invalid(
                    &a,
                    format!("Attribute {:?} is Read-Only and cannot be updated", a.path),
                ));
                return self;
            }
            if !value.is_undefined() {
                if let Err(e) = get_validate(&a, value) {
                    self.fail(e);
                    return self;
                }
            }
        }
        self
    }

    /// Mirrors `composite({...})`: values for key composites that aren't
    /// being set, added as equality conditions.
    pub fn composite(&mut self, entries: &[Entry]) -> &mut Self {
        if self.err.is_some() {
            return self;
        }
        for e in entries {
            if self.m.facets(&e.path).is_empty() {
                continue;
            }
            if !self.upd.add_composite(&e.path, &e.value) {
                self.fail(Error::invalid(format!(
                    "the composite attribute {} has been provided more than once with different values",
                    e.path
                )));
                return self;
            }
            if let Some(a) = self.m.attribute(&e.path) {
                let field = a.field.clone();
                self.cond
                    .unsafe_set(false, "eq", &field, std::slice::from_ref(&e.value));
            }
        }
        self
    }

    /// Adds a condition (ElectroDB's `where()`).
    pub fn where_(&mut self, c: &Cond) -> &mut Self {
        if self.err.is_some() {
            return self;
        }
        match self.render_cond(c) {
            Ok(expr) => self.cond.add_expr(&expr, false),
            Err(e) => self.fail(e),
        }
        self
    }

    pub(crate) fn render_cond(&mut self, c: &Cond) -> Result<String> {
        match c {
            Cond::Op { op, path, values } => apply_op(
                self.m,
                OpTarget::Filter(&mut self.cond.state),
                op,
                path,
                values,
            ),
            Cond::And(list) | Cond::Or(list) => {
                let sep = if matches!(c, Cond::And(_)) {
                    " AND "
                } else {
                    " OR "
                };
                let mut parts = Vec::with_capacity(list.len());
                for sub in list {
                    parts.push(format!("({})", self.render_cond(sub)?));
                }
                Ok(parts.join(sep))
            }
            Cond::Not(sub) => Ok(format!("NOT ({})", self.render_cond(sub)?)),
            Cond::Raw {
                expression,
                names,
                values,
            } => {
                for (k, v) in names {
                    self.cond.state.add_name(k, v);
                }
                for (k, v) in values {
                    self.cond.state.put_value(k, v.clone());
                }
                Ok(expression.clone())
            }
        }
    }

    /// Adds a raw clause to one section of the update expression (`set`,
    /// `remove`, `add`, `delete`), with its own names and values.
    pub fn raw_update(
        &mut self,
        section: &str,
        clause: &str,
        names: &IndexMap<String, String>,
        values: &Attrs,
    ) -> &mut Self {
        if self.err.is_some() {
            return self;
        }
        if !matches!(section, "set" | "remove" | "add" | "delete") {
            self.fail(Error::invalid(format!(
                "raw update: unknown section {section:?}"
            )));
            return self;
        }
        for (k, v) in names {
            self.upd.state.add_name(k, v);
        }
        for (k, v) in values {
            self.upd.state.put_value(k, v.clone());
        }
        self.upd.add(section, clause);
        self
    }

    /// Renders the request, mirroring ElectroDB's `_params`, option
    /// application and condition merging.
    pub fn build(mut self, table: &str, opt: Options) -> Result<Request<'m>> {
        if let Some(e) = self.err.take() {
            return Err(e);
        }
        let table = if opt.table.is_empty() {
            table.to_string()
        } else {
            opt.table.clone()
        };
        let mut p = Params::default();
        let target = match self.method {
            Method::Get | Method::Delete | Method::Remove | Method::Check => {
                p.key = Some(primary_key(self.m, &merge_attrs(&[&self.pk, &self.sk]))?);
                p.table_name = Some(table);
                match self.method {
                    Method::Get => Operation::GetItem,
                    Method::Check => Operation::ConditionCheck,
                    _ => Operation::DeleteItem,
                }
            }
            Method::Put | Method::Create => {
                let record = self.record.clone().unwrap_or_default();
                p.item = Some(put_item_from_record(
                    self.m,
                    &record,
                    &merge_attrs(&[&self.pk, &self.sk]),
                )?);
                p.table_name = Some(table);
                Operation::PutItem
            }
            Method::Update | Method::Patch => {
                self.build_update(&mut p, table)?;
                Operation::UpdateItem
            }
            Method::Upsert => {
                self.build_upsert(&mut p, table)?;
                Operation::UpdateItem
            }
        };

        // _applyParameterOptions
        if !opt.response.is_empty() && opt.response != "default" {
            let rv = match opt.response.as_str() {
                "none" => "NONE",
                "all_old" => "ALL_OLD",
                "updated_old" => "UPDATED_OLD",
                "all_new" => "ALL_NEW",
                "updated_new" => "UPDATED_NEW",
                r => return Err(Error::invalid(format!("invalid response option {r:?}"))),
            };
            if opt.transaction {
                p.return_values_on_condition_check_failure = Some(rv.into());
            } else {
                p.return_values = Some(rv.into());
            }
        }
        if opt.return_rejected == "all_old" {
            p.return_values_on_condition_check_failure = Some("ALL_OLD".into());
        }
        if opt.consistent {
            p.consistent_read = Some(true);
        }
        if self.method == Method::Get {
            self.apply_projection(&mut p, &opt);
        }

        // _applyParameterExpressionTypes
        if !self.cond.expression.is_empty() {
            let cond = self.cond.expression.clone();
            p.condition_expression = Some(match p.condition_expression.take() {
                Some(existing) if !existing.is_empty() => format!("{existing} AND {cond}"),
                _ => cond,
            });
            if !self.cond.state.names.is_empty() {
                let mut names = self.cond.state.names.clone();
                if let Some(existing) = p.expression_attribute_names.take() {
                    names.extend(existing);
                }
                p.expression_attribute_names = Some(names);
            }
            if !self.cond.state.values.is_empty() {
                let mut values = self.cond.state.values_attrs();
                if let Some(existing) = p.expression_attribute_values.take() {
                    values.extend(existing);
                }
                p.expression_attribute_values = Some(values);
            }
        }
        Ok(Request {
            target,
            params: p,
            w: self,
            opts: opt,
        })
    }

    /// Mirrors `_applyProjectionExpressions` for get.
    fn apply_projection(&self, p: &mut Params, opt: &Options) {
        if opt.attributes.is_empty() {
            return;
        }
        let ix = self.m.table_index();
        let mut seen = std::collections::HashMap::new();
        let mut order: Vec<String> = Vec::new();
        let mut fields: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut add =
            |name: &str, field: &str, seen: &mut std::collections::HashMap<String, String>| {
                let formatted = format_expression_name(name, seen);
                if !order.contains(&formatted) {
                    order.push(formatted.clone());
                }
                fields.insert(formatted, field.to_string());
            };
        add(&ix.pk.field, &ix.pk.field, &mut seen);
        if ix.has_sk {
            add(&ix.sk.field, &ix.sk.field, &mut seen);
        }
        for name in &opt.attributes {
            let field = self
                .m
                .attribute(name)
                .map(|a| a.field.clone())
                .unwrap_or_else(|| name.clone());
            add(&field, &field, &mut seen);
        }
        if opt.exec {
            let (e, v) = (&self.m.identifiers.entity, &self.m.identifiers.version);
            order.push(e.clone());
            order.push(v.clone());
            fields.insert(e.clone(), e.clone());
            fields.insert(v.clone(), v.clone());
        }
        let mut names = IndexMap::new();
        let mut refs = Vec::with_capacity(order.len());
        for k in &order {
            names.insert(format!("#{k}"), fields[k].clone());
            refs.push(format!("#{k}"));
        }
        p.expression_attribute_names = Some(names);
        p.projection_expression = Some(refs.join(", "));
    }

    /// Mirrors `_makeUpdateParams`.
    fn build_update(&mut self, p: &mut Params, table: String) -> Result<()> {
        let m = self.m;
        let ix = m.table_index();
        let mut modified_values = Attrs::new();
        let mut modified_names: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for (path, pv) in &self.upd.state.paths {
            modified_values.insert(path.clone(), pv.value.clone());
            modified_names.insert(path.clone(), pv.name.clone());
            order.push(path.clone());
        }
        let removed: Attrs = self
            .upd
            .state
            .impacted
            .iter()
            .filter(|(_, op)| op.as_str() == "remove")
            .map(|(path, _)| (path.clone(), Value::String(path.clone())))
            .collect();
        let key_attrs = merge_attrs(&[&self.pk, &self.sk]);
        for k in key_attrs.keys() {
            modified_values.remove(k);
            order.retain(|s| s != k);
        }
        let (prepared, prepared_order) = fulfill(m, Mutation::Set, &modified_values, &order)?;

        let keys = self.updated_keys(&prepared, &removed)?;

        for path in &prepared_order {
            let Some(v) = prepared.get(path) else {
                continue;
            };
            if v.is_undefined() {
                continue;
            }
            if let Some(name) = modified_names.get(path) {
                self.upd.state.put_value(name, v.clone());
            } else if let Some(a) = m.attribute_at_path(path) {
                let field = a.field.clone();
                self.upd.set(&field, v.clone(), false);
            } else {
                self.upd.set(path, v.clone(), false);
            }
        }
        let not_table_key = |field: &str| {
            field != ix.pk.field
                && (!ix.has_sk || field != ix.sk.field)
                && !modified_names.contains_key(field)
        };
        for field in &keys.updated_order {
            if not_table_key(field) {
                self.upd.set(field, keys.updated[field].clone(), false);
            }
        }
        for field in &keys.deleted {
            if not_table_key(field) {
                self.upd.remove(field);
            }
        }
        for name in ix.pk.composites.iter().chain(ix.sk.composites.iter()) {
            let Some(v) = key_attrs.get(name) else {
                continue;
            };
            let Some(a) = m.attribute(name) else { continue };
            if a.field != ix.pk.field
                && (!ix.has_sk || a.field != ix.sk.field)
                && !modified_names.contains_key(name)
            {
                let field = a.field.clone();
                self.upd.set(&field, v.clone(), false);
            }
        }
        self.upd.set(
            &m.identifiers.entity,
            Value::String(m.entity.clone()),
            false,
        );
        self.upd.set(
            &m.identifiers.version,
            Value::String(m.version.clone()),
            false,
        );

        p.update_expression = Some(self.upd.build());
        p.expression_attribute_names = Some(self.upd.state.names.clone());
        p.expression_attribute_values = Some(self.upd.state.values_attrs());
        p.table_name = Some(table);
        p.key = Some(keys.index_key);
        Ok(())
    }

    /// Mirrors `_getUpdatedKeys` and `_makeKeysFromAttributes`.
    fn updated_keys(&self, set: &Attrs, removed: &Attrs) -> Result<UpdatedKeys> {
        let m = self.m;
        let table = m.table_index();
        let key_attrs = merge_attrs(&[&self.sk, &self.pk]);
        let mut composites = Attrs::new();
        for n in &self.upd.comp_order {
            composites.insert(n.clone(), self.upd.composites[n].clone());
        }
        let defined: Attrs = set
            .iter()
            .filter(|(_, v)| !v.is_undefined())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let complete = expect_index_facets(
            m,
            &defined,
            &merge_attrs(&[&composites, &key_attrs]),
            ImpactOptions {
                included_only: true,
                ..Default::default()
            },
        )?;
        let removed_impact = expect_index_facets(
            m,
            removed,
            &key_attrs,
            ImpactOptions {
                skip_conditions: true,
                ..Default::default()
            },
        )?;

        let mut types = complete.types.clone();
        let mut order = complete.order.clone();
        if !types.contains_key(&table.id) {
            let mut t = std::collections::BTreeMap::new();
            t.insert(KeyType::Pk, "pk".to_string());
            t.insert(KeyType::Sk, "sk".to_string());
            types.insert(table.id, t);
            order.push(table.id);
        }
        let attrs = merge_attrs(&[&composites, &defined, &key_attrs]);

        let mut out = UpdatedKeys::default();
        for ix in &m.indexes {
            if complete.conditions.get(&ix.id) == Some(&false) {
                out.deleted.push(ix.pk.field.clone());
                if ix.has_sk {
                    out.deleted.push(ix.sk.field.clone());
                }
            }
        }
        for id in &removed_impact.order {
            for kt in [KeyType::Pk, KeyType::Sk] {
                if let Some(f) = removed_impact.types[id].get(&kt) {
                    out.deleted.push(f.clone());
                }
            }
        }

        for id in order {
            let ix = &m.indexes[id];
            let kt = &types[&id];
            if ix.ty == IndexType::Composite {
                continue;
            }
            if ix.condition.is_some()
                && !complete.conditions.get(&id).copied().unwrap_or(false)
                && id != table.id
            {
                continue;
            }
            let keys = index_keys(m, ix, &attrs, &[&attrs], KeyOptions::default())?;
            let pk = if kt.contains_key(&KeyType::Pk) {
                keys.pk.filter(truthy)
            } else {
                None
            };
            let mut sks: Vec<Value> = if kt.contains_key(&KeyType::Sk) {
                keys.sk
            } else {
                Vec::new()
            };
            if id == table.id {
                out.index_key
                    .insert(ix.pk.field.clone(), pk.clone().unwrap_or(Value::Null));
                if ix.has_sk {
                    if let Some(sk) = sks.first() {
                        out.index_key.insert(ix.sk.field.clone(), sk.clone());
                    }
                }
            } else if sks.is_empty() && ix.has_sk && ix.sk.composites.is_empty() {
                // Sparse indexes whose sk has no composites still need the sk.
                let mut key = ix.sk.prefix.clone();
                key.push_str(if ix.sk.has_postfix {
                    &ix.sk.postfix
                } else {
                    "undefined"
                });
                if !key.is_empty() {
                    sks.push(Value::String(key));
                }
            }
            if let Some(pk) = pk {
                out.updated.insert(ix.pk.field.clone(), pk);
                out.updated_order.push(ix.pk.field.clone());
            }
            if ix.has_sk {
                if let Some(sk) = sks.into_iter().next().filter(truthy) {
                    out.updated.insert(ix.sk.field.clone(), sk);
                    out.updated_order.push(ix.sk.field.clone());
                }
            }
        }
        Ok(out)
    }

    /// Mirrors the upsert clause's `beforeBuildParams` and
    /// `_makeUpsertParams`.
    fn build_upsert(&mut self, p: &mut Params, table: String) -> Result<()> {
        let m = self.m;
        self.upd.set(
            &m.identifiers.entity,
            Value::String(m.entity.clone()),
            false,
        );
        self.upd.set(
            &m.identifiers.version,
            Value::String(m.version.clone()),
            false,
        );

        let mut set_data = Attrs::new();
        let mut non_set: Vec<UpsertEntry> = Vec::new();
        let mut all_data = Attrs::new();
        for e in &self.upsert {
            all_data.insert(e.name.clone(), e.value.clone());
            if e.op == "set" {
                set_data.insert(e.name.clone(), e.value.clone());
            } else {
                non_set.push(e.clone());
            }
        }
        let upsert_data = check_create(m, &all_data)?;
        self.key_facets(&upsert_data);
        if let Some(e) = self.err.take() {
            return Err(e);
        }
        // ElectroDB adds the key composites to the response options after
        // they were read, so an upsert's default result is an empty object.
        self.include = Attrs::new();
        let (applied, _) = fulfill(m, Mutation::Set, &upsert_data, &root_names(m))?;
        let mut only_set = Attrs::new();
        for a in &m.attributes {
            let Some(v) = applied.get(&a.name) else {
                continue;
            };
            let is_set = set_data.contains_key(&a.name);
            let provided = all_data.contains_key(&a.name);
            if (is_set || !provided) && !v.is_undefined() {
                only_set.insert(a.name.clone(), v.clone());
            }
        }

        let (updated_keys, deleted_keys) =
            put_keys_deleted(m, &only_set, &merge_attrs(&[&self.sk, &self.pk]))?;
        for k in &deleted_keys {
            self.upd.remove(k);
        }
        let key = primary_key(m, &merge_attrs(&[&self.pk, &self.sk]))?;

        // Set attributes come back in attribute order; then index keys.
        let mut set_fields: Vec<(String, Value)> = Vec::new();
        for a in &m.attributes {
            if let Some(v) = only_set.get(&a.name) {
                set_fields.push((a.field.clone(), v.clone()));
            }
        }
        for ix in &m.indexes {
            for f in [&ix.pk.field, &ix.sk.field] {
                let Some(v) = updated_keys.get(f) else {
                    continue;
                };
                if key.contains_key(f) {
                    set_fields.retain(|(name, _)| name != f);
                    continue;
                }
                let mut found = false;
                for sf in set_fields.iter_mut() {
                    if sf.0 == *f {
                        sf.1 = v.clone();
                        found = true;
                    }
                }
                if !found {
                    set_fields.push((f.clone(), v.clone()));
                }
            }
        }
        for (field, value) in set_fields {
            self.apply_upsert_field(&field, value, "set")?;
        }
        for e in non_set {
            let Some(v) = applied.get(&e.name) else {
                continue;
            };
            if v.is_undefined() {
                continue;
            }
            let Some(a) = m.attribute(&e.name) else {
                continue;
            };
            let field = a.field.clone();
            self.apply_upsert_field(&field, v.clone(), &e.op)?;
        }

        p.table_name = Some(table);
        p.update_expression = Some(self.upd.build());
        p.expression_attribute_names = Some(self.upd.state.names.clone());
        p.expression_attribute_values = Some(self.upd.state.values_attrs());
        p.key = Some(key);
        Ok(())
    }

    /// Mirrors `_maybeApplyUpsertUpdate`.
    fn apply_upsert_field(&mut self, field: &str, value: Value, op: &str) -> Result<()> {
        let attr = self
            .m
            .attributes
            .iter()
            .rev()
            .find(|a| a.field == field)
            .cloned();
        let Some(attr) = attr else {
            self.upd.set(field, value, op == "ifNotExists");
            return Ok(());
        };
        if attr.read_only && self.m.facets(&attr.name).is_empty() {
            self.upd.set(field, value, true);
            return Ok(());
        }
        self.perform_operation(op, &Path(vec![attr.name.clone()]), value, true)
    }
}

#[derive(Default)]
struct UpdatedKeys {
    index_key: Attrs,
    updated: Attrs,
    updated_order: Vec<String>,
    deleted: Vec<String>,
}

/// Mirrors JS truthiness for key values.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Undefined => false,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_str() != "0" && n.as_str() != "-0",
        _ => true,
    }
}

/// A built single-item request.
#[derive(Clone)]
pub struct Request<'m> {
    pub target: Operation,
    pub params: Params,
    w: Write<'m>,
    opts: Options,
}

/// The item fields of a DynamoDB response.
#[derive(Clone, Debug, Default)]
pub struct Returned {
    pub item: Option<Attrs>,
    pub attributes: Option<Attrs>,
}

/// A formatted single-item result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Response {
    /// The formatted item; `None` when ElectroDB returns null (or nothing).
    pub data: Option<Attrs>,
    pub null: bool,
    /// Set when `return_rejected` was requested and the condition failed.
    pub rejected: bool,
}

impl<'m> Request<'m> {
    pub fn model(&self) -> &'m Model {
        self.w.m
    }

    pub fn options(&self) -> &Options {
        &self.opts
    }

    /// Reports whether condition failures should be returned as a rejected
    /// result instead of an error.
    pub fn reject_requested(&self) -> bool {
        !self.opts.return_rejected.is_empty()
    }

    /// Mirrors `executeOperation`'s formatting of a successful response.
    pub fn format(&self, resp: &Returned) -> Result<Response> {
        match self.params.return_values.as_deref() {
            Some("NONE") => {
                return Ok(Response {
                    null: true,
                    ..Default::default()
                });
            }
            Some("ALL_NEW" | "ALL_OLD" | "UPDATED_NEW" | "UPDATED_OLD") => {
                return self.format_response(resp, false);
            }
            _ => {}
        }
        match self.w.method {
            Method::Put | Method::Create => self.format_response(
                &Returned {
                    item: self.params.item.clone(),
                    attributes: None,
                },
                false,
            ),
            Method::Get => self.format_response(resp, false),
            _ => self.format_response(resp, true),
        }
    }

    /// Mirrors the condition-failure branch of `executeOperation`. `item` is
    /// the returned old item, if any.
    pub fn format_rejected(&self, item: Option<&Attrs>) -> Result<Response> {
        let all_old = self.opts.return_rejected == "all_old";
        let Some(item) = item.filter(|_| all_old) else {
            return Ok(Response {
                rejected: true,
                null: all_old,
                data: None,
            });
        };
        let mut resp = self.format_response(
            &Returned {
                item: Some(item.clone()),
                attributes: None,
            },
            false,
        )?;
        resp.rejected = true;
        Ok(resp)
    }

    fn format_response(&self, resp: &Returned, object_on_empty: bool) -> Result<Response> {
        let m = self.w.m;
        let opt = FormatOptions {
            include_keys: self.opts.include_keys,
            attributes: self.opts.attributes.clone(),
        };
        if let Some(item) = &resp.item {
            if owns_item(m, item) {
                return Ok(Response {
                    data: Some(format_item(m, item, &opt)?),
                    ..Default::default()
                });
            }
            if object_on_empty {
                return Ok(Response {
                    data: Some(Attrs::new()),
                    ..Default::default()
                });
            }
            return Ok(Response {
                null: true,
                ..Default::default()
            });
        }
        if let Some(attrs) = &resp.attributes {
            let data = format_item(m, attrs, &opt)?;
            if data.is_empty() && !object_on_empty {
                return Ok(Response {
                    null: true,
                    ..Default::default()
                });
            }
            return Ok(Response {
                data: Some(data),
                ..Default::default()
            });
        }
        if object_on_empty {
            return Ok(Response {
                data: Some(self.w.include.clone()),
                ..Default::default()
            });
        }
        Ok(Response {
            null: true,
            ..Default::default()
        })
    }
}
