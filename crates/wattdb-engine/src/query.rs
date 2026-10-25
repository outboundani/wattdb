//! Queries and scans (ElectroDB's query clauses), and collection queries
//! (service.js `_makeCollectionChain`).

use indexmap::IndexMap;

use crate::error::{Error, Result};
use crate::expr::{ExprState, FilterExpr, format_expression_name};
use crate::keys::{IndexKeys, KeyOptions, QueryType, index_keys};
use crate::model::{Index, IndexType, Model, format_casing};
use crate::request::{Operation, Params};
use crate::value::{Attrs, Value};
use crate::write::{Cond, Entry, Write, expect_facets};

/// The options that shape query and scan requests and their results.
#[derive(Clone, Debug, Default)]
pub struct QueryOptions {
    /// Overrides the table.
    pub table: String,
    pub limit: i64,
    /// `""`, `"asc"` or `"desc"`.
    pub order: String,
    pub consistent: bool,
    pub attributes: Vec<String>,
    pub ignore_ownership: bool,
    /// `"keys"` (the default) or `"attributes"` (decision D51).
    pub compare: String,
    pub raw: bool,
    pub include_keys: bool,
    pub hydrate: bool,
    /// True when the request is sent rather than rendered.
    pub exec: bool,
}

#[derive(Clone, Debug)]
struct SkEntry {
    typ: QueryType,
    facets: Vec<Entry>,
}

/// A filter that depends on the options, added when the request is built
/// (ElectroDB's `beforeBuildParams`).
#[derive(Clone, Debug)]
enum Before {
    Compare {
        typ: QueryType,
        composites: Vec<Entry>,
    },
    Between {
        start: Vec<Entry>,
        end: Vec<Entry>,
    },
}

/// One entity of a collection, under its service alias.
#[derive(Clone)]
pub struct CollectionMember<'m> {
    pub alias: String,
    pub model: &'m Model,
}

#[derive(Clone)]
pub(crate) struct Collection<'m> {
    pub members: Vec<CollectionMember<'m>>,
    /// The collection's sort-key prefix (`_getCollectionSk`).
    pub sk: String,
}

/// Builds a query or scan.
#[derive(Clone)]
pub struct Query<'m> {
    pub(crate) m: &'m Model,
    pub(crate) ix: usize,
    pub(crate) scan: bool,
    typ: QueryType,
    pk: Attrs,
    sk: Vec<SkEntry>,
    /// The facets passed to the access pattern.
    given: Vec<Entry>,
    filter: FilterExpr,
    before: Vec<Before>,
    err: Option<Error>,
    /// The sk composites given, for "is" fulfilment.
    provided: Vec<String>,
    pub(crate) coll: Option<Collection<'m>>,
    /// For collections, the members' merged attributes, used by filters.
    filter_model: Option<Box<Model>>,
}

fn entries_to_attrs(es: &[Entry]) -> Attrs {
    es.iter()
        .filter(|e| !e.value.is_undefined())
        .map(|e| (e.path.clone(), e.value.clone()))
        .collect()
}

impl<'m> Query<'m> {
    fn blank(m: &'m Model, ix: usize, scan: bool) -> Self {
        Query {
            m,
            ix,
            scan,
            typ: QueryType::Is,
            pk: Attrs::new(),
            sk: Vec::new(),
            given: Vec::new(),
            filter: FilterExpr::new(),
            before: Vec::new(),
            err: None,
            provided: Vec::new(),
            coll: None,
            filter_model: None,
        }
    }

    fn fail(&mut self, err: Error) {
        if self.err.is_none() {
            self.err = Some(err);
        }
    }

    pub(crate) fn index(&self) -> &'m Index {
        &self.m.indexes[self.ix]
    }

    /// The entries naming sk composites, in the given order
    /// (identifyCompositeAttributes' "composites").
    fn sk_composites(&self, es: &[Entry]) -> Vec<Entry> {
        let ix = self.index();
        es.iter()
            .filter(|e| ix.sk.composites.contains(&e.path))
            .cloned()
            .collect()
    }

    /// The defined sk composites in sk order (buildQueryComposites).
    fn sk_ordered(&self, es: &[Entry]) -> Vec<Entry> {
        let given = entries_to_attrs(es);
        self.index()
            .sk
            .composites
            .iter()
            .filter_map(|n| given.get(n).map(|v| Entry::new(n.clone(), v.clone())))
            .collect()
    }

    fn set_sk(&mut self, typ: QueryType, es: Vec<Entry>) {
        if !self.index().has_sk {
            return;
        }
        for e in &es {
            if !self.provided.contains(&e.path) {
                self.provided.push(e.path.clone());
            }
        }
        self.sk.push(SkEntry { typ, facets: es });
    }

    /// Starts a query on an access pattern with the given composites, in the
    /// order given.
    pub fn new(m: &'m Model, access_pattern: &str, facets: Vec<Entry>) -> Self {
        let Some(ix) = m.access_pattern(access_pattern) else {
            let mut q = Query::blank(m, 0, false);
            q.fail(Error::invalid(format!(
                "unknown access pattern {access_pattern:?}"
            )));
            return q;
        };
        let mut q = Query::blank(m, ix.id, false);
        q.given = facets;
        match expect_facets(&entries_to_attrs(&q.given), &ix.pk.composites) {
            Ok(pk) => q.pk = pk,
            Err(e) => {
                q.fail(e);
                return q;
            }
        }
        let sk = q.sk_ordered(&q.given);
        q.set_sk(QueryType::Is, sk);
        q
    }

    /// Starts a scan of the table, or of an access pattern's index.
    pub fn scan(m: &'m Model, access_pattern: &str) -> Self {
        if access_pattern.is_empty() {
            return Query::blank(m, m.table_index().id, true);
        }
        match m.access_pattern(access_pattern) {
            Some(ix) => Query::blank(m, ix.id, true),
            None => {
                let mut q = Query::blank(m, 0, true);
                q.fail(Error::invalid(format!(
                    "unknown access pattern {access_pattern:?}"
                )));
                q
            }
        }
    }

    fn filter_props(&mut self, op: &str, es: &[Entry], as_prefix: bool) {
        for e in es {
            if e.value.is_undefined() {
                continue;
            }
            if let Some(a) = self.m.attribute(&e.path) {
                let field = a.field.clone();
                self.filter
                    .unsafe_set(as_prefix, op, &field, std::slice::from_ref(&e.value));
            }
        }
    }

    /// Mirrors `begins()`.
    pub fn begins(&mut self, facets: &[Entry]) -> &mut Self {
        if self.err.is_some() {
            return self;
        }
        self.typ = QueryType::Begins;
        let sk = self.sk_ordered(facets);
        self.set_sk(QueryType::Begins, sk);
        self
    }

    /// Mirrors `gt`/`gte`/`lt`/`lte`.
    pub fn compare(&mut self, typ: QueryType, facets: &[Entry]) -> &mut Self {
        if self.err.is_some() {
            return self;
        }
        self.typ = typ;
        let composites = self.sk_composites(facets);
        let sk = if typ == QueryType::Gte {
            self.sk_ordered(facets)
        } else {
            composites.clone()
        };
        self.set_sk(typ, sk);
        self.before.push(Before::Compare { typ, composites });
        self
    }

    /// Mirrors `between()`.
    pub fn between(&mut self, start: &[Entry], end: &[Entry]) -> &mut Self {
        if self.err.is_some() {
            return self;
        }
        self.typ = QueryType::Between;
        let end_sk = self.sk_composites(end);
        let start_sk = self.sk_composites(start);
        self.set_sk(QueryType::And, end_sk.clone());
        self.set_sk(QueryType::Between, start_sk.clone());
        self.before.push(Before::Between {
            start: start_sk,
            end: end_sk,
        });
        self
    }

    /// Adds a filter.
    pub fn where_(&mut self, c: &Cond) -> &mut Self {
        if self.err.is_some() {
            return self;
        }
        let filter = std::mem::take(&mut self.filter);
        let m: &Model = self.filter_model.as_deref().unwrap_or(self.m);
        let mut w = Write::for_filter(m, filter);
        let r = w.render_cond(c);
        let mut filter = w.into_filter();
        match r {
            Ok(expr) => filter.add_expr(&expr, false),
            Err(e) => {
                self.filter = filter;
                self.fail(e);
                return self;
            }
        }
        self.filter = filter;
        self
    }

    /// Mirrors `_consolidateQueryFacets`.
    fn consolidate(&self) -> (Attrs, Attrs) {
        let (mut sk1, mut sk2) = (Attrs::new(), Attrs::new());
        for e in &self.sk {
            for f in &e.facets {
                match e.typ {
                    QueryType::Between => {
                        sk1.insert(f.path.clone(), f.value.clone());
                    }
                    QueryType::And => {
                        sk2.insert(f.path.clone(), f.value.clone());
                    }
                    _ => {
                        sk1.insert(f.path.clone(), f.value.clone());
                        sk2.insert(f.path.clone(), f.value.clone());
                    }
                }
            }
        }
        (sk1, sk2)
    }

    fn run_before(&mut self, opt: &QueryOptions) {
        let ix = self.index();
        for b in self.before.clone() {
            if opt.compare != "attributes" || ix.sk.field_ref {
                continue;
            }
            match b {
                Before::Compare { typ, composites } => {
                    if typ == QueryType::Gte && ix.sk.composites.len() <= 1 {
                        continue;
                    }
                    let op = match typ {
                        QueryType::Gt => "gt",
                        QueryType::Gte => "gte",
                        QueryType::Lt => "lt",
                        _ => "lte",
                    };
                    self.filter_props(op, &composites, true);
                }
                Before::Between { start, end } => {
                    self.filter_props("lte", &end, true);
                    self.filter_props("gte", &start, true);
                }
            }
        }
    }

    /// Renders the request (`_queryParams` / `_makeScanParam`).
    pub fn build(mut self, table: &str, opt: QueryOptions) -> Result<QueryRequest<'m>> {
        if let Some(e) = self.err.take() {
            return Err(e);
        }
        let table = if opt.table.is_empty() {
            table.to_string()
        } else {
            opt.table.clone()
        };
        let ix = self.index();
        let m = self.m;
        // Options-dependent filters, in ElectroDB's order: whenOptions (from
        // the access pattern and scan), then beforeBuildParams (sort-key ops).
        if self.scan {
            if !opt.ignore_ownership {
                self.filter.unsafe_set(
                    false,
                    "eq",
                    &m.identifiers.entity,
                    &[Value::String(m.entity.clone())],
                );
                self.filter.unsafe_set(
                    false,
                    "eq",
                    &m.identifiers.version,
                    &[Value::String(m.version.clone())],
                );
            }
        } else {
            let given = entries_to_attrs(&self.given);
            let mut composites = Vec::new();
            let mut unused = Vec::new();
            for e in &self.given {
                if ix.sk.composites.contains(&e.path) {
                    composites.push(e.clone());
                } else if !ix.pk.composites.contains(&e.path) {
                    unused.push(e.clone());
                }
            }
            if ix.has_sk {
                if opt.compare == "attributes" && ix.sk.composites.len() > 1 {
                    let mut es = unused.clone();
                    es.extend(composites.clone());
                    self.filter_props("eq", &es, false);
                }
                let found = ix
                    .sk
                    .composites
                    .iter()
                    .filter(|n| given.contains_key(*n))
                    .count();
                if self.coll.is_none()
                    && ix.ty == IndexType::Clustered
                    && found < ix.sk.composites.len()
                    && !opt.ignore_ownership
                {
                    self.filter.unsafe_set(
                        false,
                        "eq",
                        &m.identifiers.entity,
                        &[Value::String(m.entity.clone())],
                    );
                    self.filter.unsafe_set(
                        false,
                        "eq",
                        &m.identifiers.version,
                        &[Value::String(m.version.clone())],
                    );
                }
            }
            self.run_before(&opt);
        }

        let mut names: IndexMap<String, String> = self.filter.state.names.clone();
        let mut values = self.filter.state.values_attrs();
        let mut custom = String::new();
        if self.coll.is_some() && !opt.ignore_ownership {
            let (es, expr) = self.collection_filter();
            custom = expr;
            for (k, v) in &es.names {
                names.insert(k.clone(), v.clone());
            }
            values.extend(es.values_attrs());
        }
        let mut p = Params::default();
        let target = if self.scan {
            self.build_scan(&mut p, &mut names, &mut values)?;
            Operation::Scan
        } else if ix.ty == IndexType::Composite {
            self.build_composite(&mut p, &mut names, &mut values, &opt, &custom)?;
            Operation::Query
        } else {
            self.build_query(&mut p, &mut names, &mut values, &custom)?;
            Operation::Query
        };
        p.table_name = Some(table);
        if !ix.name.is_empty() {
            p.index_name = Some(ix.name.clone());
        }
        if opt.limit > 0 {
            p.limit = Some(opt.limit);
        }
        match opt.order.as_str() {
            "asc" => p.scan_index_forward = Some(true),
            "desc" => p.scan_index_forward = Some(false),
            _ => {}
        }
        if opt.consistent {
            p.consistent_read = Some(true);
        }
        if !opt.attributes.is_empty() {
            p.projection_expression = Some(projection(
                m,
                &mut names,
                &opt.attributes,
                !opt.raw && opt.exec && !opt.ignore_ownership,
            ));
        }
        if !names.is_empty() {
            p.expression_attribute_names = Some(names);
        }
        if !values.is_empty() {
            p.expression_attribute_values = Some(values);
        }
        Ok(QueryRequest {
            target,
            params: p,
            q: self,
            opts: opt,
        })
    }

    fn build_query(
        &self,
        p: &mut Params,
        names: &mut IndexMap<String, String>,
        values: &mut Attrs,
        custom: &str,
    ) -> Result<()> {
        let ix = self.index();
        let (sk1, sk2) = self.consolidate();
        let opt = if self.typ == QueryType::Is {
            KeyOptions::default()
        } else {
            KeyOptions {
                exclude_label_tail: true,
                exclude_postfix: self.coll.is_some() && ix.ty == IndexType::Clustered,
            }
        };
        let keys: IndexKeys = index_keys(self.m, ix, &self.pk, &[&sk1, &sk2], opt)?;
        names.insert("#pk".into(), ix.pk.field.clone());
        values.insert(":pk".into(), keys.pk.clone().unwrap_or(Value::Null));
        let sk = |i: usize| keys.sk.get(i).cloned().unwrap_or(Value::Undefined);

        let mut condition = String::from("#pk = :pk");
        let mut inclusive = |is: bool, names: &mut IndexMap<String, String>, values: &mut Attrs| {
            let v = match (&self.typ, &self.coll) {
                (QueryType::Collection, Some(c)) => Value::String(c.sk.clone()),
                _ => sk(0),
            };
            let usable = match &v {
                Value::Number(_) => true,
                Value::String(s) => !s.is_empty(),
                _ => false,
            };
            if ix.has_sk && usable {
                names.insert("#sk1".into(), ix.sk.field.clone());
                values.insert(":sk1".into(), v);
                condition.push_str(if is {
                    " and #sk1 = :sk1"
                } else {
                    " and begins_with(#sk1, :sk1)"
                });
            }
        };
        match self.typ {
            QueryType::Is => {
                let fulfilled = self.provided.len() == ix.sk.composites.len()
                    && ix.sk.composites.iter().all(|n| self.provided.contains(n));
                inclusive(fulfilled, names, values);
            }
            QueryType::Begins | QueryType::Collection | QueryType::ClusteredCollection => {
                inclusive(false, names, values)
            }
            QueryType::Between => {
                names.insert("#sk1".into(), ix.sk.field.clone());
                values.insert(":sk1".into(), sk(0));
                values.insert(":sk2".into(), sk(1));
                condition.push_str(" and #sk1 BETWEEN :sk1 AND :sk2");
            }
            QueryType::Gt | QueryType::Gte | QueryType::Lt | QueryType::Lte => {
                names.insert("#sk1".into(), ix.sk.field.clone());
                values.insert(":sk1".into(), sk(0));
                condition.push_str(" and #sk1 ");
                condition.push_str(op_symbol(self.typ));
                condition.push_str(" :sk1");
            }
            QueryType::And => {}
        }
        p.key_condition_expression = Some(condition);
        let f = join_filters(&[custom, &self.filter.expression]);
        if !f.is_empty() {
            p.filter_expression = Some(f);
        }
        Ok(())
    }

    /// Mirrors `_compositeQueryParams`: key conditions on the composite
    /// attributes themselves.
    fn build_composite(
        &self,
        p: &mut Params,
        names: &mut IndexMap<String, String>,
        values: &mut Attrs,
        opt: &QueryOptions,
        custom: &str,
    ) -> Result<()> {
        let ix = self.index();
        let m = self.m;
        let mut es = ExprState::new("k_");
        let mut exprs: Vec<String> = Vec::new();
        let field = |name: &str| {
            m.attribute(name)
                .map(|a| a.field.clone())
                .unwrap_or_else(|| name.to_string())
        };
        let eq = |es: &mut ExprState, exprs: &mut Vec<String>, name: &str, v: Value| {
            let r = es.set_name(None, name, &field(name)).expression;
            let val = es.set_value(name, v);
            exprs.push(format!("{r} = {val}"));
        };
        if self.typ == QueryType::Between {
            for name in &ix.pk.composites {
                eq(
                    &mut es,
                    &mut exprs,
                    name,
                    self.pk.get(name).cloned().unwrap_or_default(),
                );
            }
            let (mut is, mut start, mut end) = (Attrs::new(), Attrs::new(), Attrs::new());
            for e in &self.sk {
                match e.typ {
                    QueryType::Is => is = entries_to_attrs(&e.facets),
                    QueryType::Between => start = entries_to_attrs(&e.facets),
                    QueryType::And => end = entries_to_attrs(&e.facets),
                    _ => {}
                }
            }
            let mut last = String::new();
            for name in &ix.sk.composites {
                let (in_is, in_start, in_end) = (
                    is.contains_key(name),
                    start.contains_key(name),
                    end.contains_key(name),
                );
                if in_is || (in_start && in_end) {
                    last = name.clone();
                } else if in_start || in_end {
                    return Err(Error::invalid(format!(
                        "between on a composite index needs both a start and an end value for {name}"
                    )));
                }
                if !in_is && !in_start && !in_end {
                    break;
                }
            }
            for name in &ix.sk.composites {
                let (in_is, in_start, in_end) = (
                    is.contains_key(name),
                    start.contains_key(name),
                    end.contains_key(name),
                );
                if *name == last {
                    let r = es.set_name(None, name, &field(name)).expression;
                    let lo = es.set_value(name, start.get(name).cloned().unwrap_or_default());
                    let hi = es.set_value(name, end.get(name).cloned().unwrap_or_default());
                    exprs.push(format!("{r} BETWEEN {lo} AND {hi}"));
                } else if in_is {
                    eq(&mut es, &mut exprs, name, is[name].clone());
                } else if in_start && in_end {
                    if !same_value(&start[name], &end[name]) {
                        return Err(Error::invalid(format!(
                            "between on a composite index needs equal start and end values for {name} until the last sort key attribute"
                        )));
                    }
                    eq(&mut es, &mut exprs, name, start[name].clone());
                }
            }
        } else {
            let (sk1, _) = self.consolidate();
            let mut parts: Vec<(bool, String, Value)> = Vec::new();
            for (i, name) in ix.composites().into_iter().enumerate() {
                let is_sk = i >= ix.pk.composites.len();
                let v = if is_sk {
                    sk1.get(&name)
                } else {
                    self.pk.get(&name)
                };
                let Some(v) = v else { break };
                parts.push((is_sk, name, v.clone()));
            }
            let n = parts.len();
            for (i, (is_sk, name, v)) in parts.into_iter().enumerate() {
                if !is_sk || i < n - 1 {
                    eq(&mut es, &mut exprs, &name, v);
                    continue;
                }
                let r = es.set_name(None, &name, &field(&name)).expression;
                let val = es.set_value(&name, v);
                match self.typ {
                    QueryType::Is | QueryType::Collection | QueryType::ClusteredCollection => {
                        exprs.push(format!("{r} = {val}"))
                    }
                    QueryType::Begins => exprs.push(format!("begins_with({r}, {val})")),
                    QueryType::Gt | QueryType::Gte | QueryType::Lt | QueryType::Lte => {
                        exprs.push(format!("{r} {} {val}", op_symbol(self.typ)))
                    }
                    _ => {}
                }
            }
        }
        let identifiers = if !opt.ignore_ownership && custom.is_empty() {
            identifier_expr(m, &mut es, "")
        } else {
            String::new()
        };
        for (k, v) in &es.names {
            names.insert(k.clone(), v.clone());
        }
        values.extend(es.values_attrs());
        p.key_condition_expression = Some(exprs.join(" AND "));
        let f = join_filters(&[custom, &self.filter.expression, &identifiers]);
        if !f.is_empty() {
            p.filter_expression = Some(f);
        }
        Ok(())
    }

    fn build_scan(
        &self,
        p: &mut Params,
        names: &mut IndexMap<String, String>,
        values: &mut Attrs,
    ) -> Result<()> {
        let ix = self.index();
        let empty = Attrs::new();
        let keys = index_keys(self.m, ix, &empty, &[&empty], KeyOptions::default())?;
        let mut filters = Vec::new();
        let mut add = |field: &str, v: Option<&Value>| {
            let Some(v) = v else { return };
            if matches!(v, Value::Null | Value::Undefined) || *v == Value::String(String::new()) {
                return;
            }
            names.insert(format!("#{field}"), field.to_string());
            values.insert(format!(":{field}"), v.clone());
            filters.push(format!("begins_with(#{field}, :{field})"));
        };
        add(&ix.pk.field, keys.pk.as_ref());
        if ix.has_sk {
            add(&ix.sk.field, keys.sk.first());
        }
        if !self.filter.expression.is_empty() {
            filters.push(self.filter.expression.clone());
        }
        if !filters.is_empty() {
            p.filter_expression = Some(filters.join(" AND "));
        }
        Ok(())
    }

    /// Starts a collection query. The members are in service order; the
    /// first one's index builds the keys.
    pub fn collection(
        name: &str,
        members: Vec<CollectionMember<'m>>,
        facets: Vec<Entry>,
    ) -> Result<Self> {
        let Some(first) = members.first() else {
            return Err(Error::invalid(format!(
                "collection {name:?} has no entities"
            )));
        };
        let m = first.model;
        let Some((ix, path)) = m.collection_index(name) else {
            return Err(Error::invalid(format!("unknown collection {name:?}")));
        };
        let models: Vec<&Model> = members.iter().map(|mem| mem.model).collect();
        let mut q = Query::blank(m, ix.id, false);
        q.given = facets;
        q.filter_model = Some(Box::new(Model::merged(&models)));
        let sk = if ix.sk.custom {
            String::new()
        } else {
            format_casing(&format!("${}", path.join("#")), ix.sk.casing)
        };
        q.coll = Some(Collection { members, sk });
        match expect_facets(&entries_to_attrs(&q.given), &ix.pk.composites) {
            Ok(pk) => q.pk = pk,
            Err(e) => {
                q.fail(e);
                return Ok(q);
            }
        }
        if ix.ty == IndexType::Isolated {
            q.typ = QueryType::Collection;
        } else {
            q.typ = QueryType::ClusteredCollection;
            let sk = q.sk_composites(&q.given);
            q.set_sk(QueryType::ClusteredCollection, sk);
        }
        Ok(q)
    }

    /// Mirrors the identifiers custom expression of collection queries: one
    /// identifier check per member, ORed.
    fn collection_filter(&self) -> (ExprState, String) {
        let mut es = ExprState::new("_c");
        let coll = self.coll.as_ref().expect("collection query");
        let parts: Vec<String> = coll
            .members
            .iter()
            .map(|mem| identifier_expr(mem.model, &mut es, &mem.alias))
            .collect();
        let mut expr = parts.join(" OR ");
        if parts.len() > 1 {
            expr = format!("({expr})");
        }
        (es, expr)
    }

    /// Mirrors find: a query on the best-matching access pattern, or a scan
    /// of the table when none fits.
    pub fn find(m: &'m Model, facets: Vec<Entry>) -> Self {
        match crate::conv::best_index(m, &entries_to_attrs(&facets)) {
            Some(pattern) => Query::new(m, &pattern, facets),
            None => Query::scan(m, ""),
        }
    }

    /// Mirrors match: find, filtered to items equal to every given attribute.
    pub fn match_(m: &'m Model, facets: Vec<Entry>) -> Self {
        let mut q = Query::find(m, facets.clone());
        if q.err.is_some() {
            return q;
        }
        let filter = std::mem::take(&mut q.filter);
        let mut w = Write::for_filter(m, filter);
        let mut parts = Vec::new();
        for e in &facets {
            if e.value.is_undefined() || m.attribute(&e.path).is_none() {
                continue;
            }
            let c = Cond::Op {
                op: "eq".into(),
                path: crate::ops::Path(vec![e.path.clone()]),
                values: vec![e.value.clone()],
            };
            match w.render_cond(&c) {
                Ok(expr) => parts.push(expr),
                Err(err) => {
                    q.filter = w.into_filter();
                    q.fail(err);
                    return q;
                }
            }
        }
        let mut filter = w.into_filter();
        filter.add_expr(&parts.join(" AND "), false);
        q.filter = filter;
        q
    }
}

fn op_symbol(typ: QueryType) -> &'static str {
    match typ {
        QueryType::Gt => ">",
        QueryType::Gte => ">=",
        QueryType::Lt => "<",
        _ => "<=",
    }
}

fn join_filters(exprs: &[&str]) -> String {
    exprs
        .iter()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty())
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Mirrors `applyIdentifierExpressionState`.
fn identifier_expr(m: &Model, es: &mut ExprState, alias: &str) -> String {
    let alias = if alias.is_empty() {
        m.entity.as_str()
    } else {
        alias
    };
    let e = es
        .set_name(None, &m.identifiers.entity, &m.identifiers.entity)
        .expression;
    let v = es
        .set_name(None, &m.identifiers.version, &m.identifiers.version)
        .expression;
    let ev = es.set_value(
        &format!("{}_{alias}", m.identifiers.entity),
        Value::String(m.entity.clone()),
    );
    let vv = es.set_value(
        &format!("{}_{alias}", m.identifiers.version),
        Value::String(m.version.clone()),
    );
    format!("({e} = {ev} AND {v} = {vv})")
}

/// Renders a ProjectionExpression over the existing names, the table key
/// fields, the attributes and optionally the identifiers.
pub(crate) fn projection(
    m: &Model,
    names: &mut IndexMap<String, String>,
    attributes: &[String],
    identifiers: bool,
) -> String {
    let table = m.table_index();
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut order: IndexMap<String, String> = IndexMap::new();
    let mut has_pk = false;
    let mut has_sk = !table.has_sk;
    for (k, v) in names.iter() {
        let key = k.strip_prefix('#').unwrap_or(k).to_string();
        seen.insert(key.clone(), v.clone());
        order.insert(key, v.clone());
        if *v == table.pk.field {
            has_pk = true;
        }
        if table.has_sk && *v == table.sk.field {
            has_sk = true;
        }
    }
    let mut set = |seen: &mut std::collections::HashMap<String, String>, field: &str| {
        let k = format_expression_name(field, seen);
        seen.insert(k.clone(), field.to_string());
        order.insert(k, field.to_string());
    };
    if !has_pk {
        set(&mut seen, &table.pk.field);
    }
    if !has_sk {
        set(&mut seen, &table.sk.field);
    }
    for name in attributes {
        let field = m
            .attribute(name)
            .map(|a| a.field.clone())
            .unwrap_or_else(|| name.clone());
        set(&mut seen, &field);
    }
    if identifiers {
        order.insert(m.identifiers.entity.clone(), m.identifiers.entity.clone());
        order.insert(m.identifiers.version.clone(), m.identifiers.version.clone());
    }
    let mut refs = Vec::with_capacity(order.len());
    for (k, v) in order {
        names.insert(format!("#{k}"), v);
        refs.push(format!("#{k}"));
    }
    refs.join(", ")
}

/// Mirrors JS strict equality on composite values.
fn same_value(a: &Value, b: &Value) -> bool {
    if let (Value::Number(x), Value::Number(y)) = (a, b) {
        return x.as_f64() == y.as_f64();
    }
    a == b
}

/// A built query or scan.
#[derive(Clone)]
pub struct QueryRequest<'m> {
    pub target: Operation,
    pub params: Params,
    pub(crate) q: Query<'m>,
    pub(crate) opts: QueryOptions,
}

impl<'m> QueryRequest<'m> {
    /// The request's model (a collection's first member).
    pub fn model(&self) -> &'m Model {
        self.q.m
    }

    pub fn options(&self) -> &QueryOptions {
        &self.opts
    }

    /// Reports whether the request is a collection query.
    pub fn is_collection(&self) -> bool {
        self.q.coll.is_some()
    }

    /// A collection query's members.
    pub fn members(&self) -> &[CollectionMember<'m>] {
        self.q
            .coll
            .as_ref()
            .map(|c| c.members.as_slice())
            .unwrap_or(&[])
    }

    /// The params with an ExclusiveStartKey.
    pub fn with_start_key(&self, key: Option<&Attrs>) -> Params {
        let mut p = self.params.clone();
        if let Some(k) = key.filter(|k| !k.is_empty()) {
            p.exclusive_start_key = Some(k.clone());
        }
        p
    }
}
