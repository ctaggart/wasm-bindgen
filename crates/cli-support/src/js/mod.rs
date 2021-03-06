use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::mem;

use failure::{Error, ResultExt};
use parity_wasm;
use parity_wasm::elements::*;
use shared;
use wasm_gc;

use super::Bindgen;
use descriptor::{Descriptor, VectorKind};
use wasm_interpreter::Interpreter;

mod js2rust;
use self::js2rust::Js2Rust;
mod rust2js;
use self::rust2js::Rust2Js;
mod closures;

pub struct Context<'a> {
    pub globals: String,
    pub imports: String,
    pub footer: String,
    pub typescript: String,
    pub exposed_globals: HashSet<&'static str>,
    pub required_internal_exports: HashSet<&'static str>,
    pub imported_functions: HashSet<String>,
    pub imported_statics: HashSet<String>,
    pub config: &'a Bindgen,
    pub module: &'a mut Module,

    /// A map which maintains a list of what identifiers we've imported and what
    /// they're named locally.
    ///
    /// The `Option<String>` key is the module that identifiers were imported
    /// from, `None` being the global module. The second key is a map of
    /// identifiers we've already imported from the module to what they're
    /// called locally.
    pub imported_names: HashMap<Option<String>, HashMap<String, String>>,

    /// A set of all imported identifiers to the number of times they've been
    /// imported, used to generate new identifiers.
    pub imported_identifiers: HashMap<String, usize>,

    pub exported_classes: HashMap<String, ExportedClass>,
    pub function_table_needed: bool,
    pub interpreter: &'a mut Interpreter,
    pub memory_init: Option<ResizableLimits>,
}

#[derive(Default)]
pub struct ExportedClass {
    comments: String,
    contents: String,
    typescript: String,
    has_constructor: bool,
    wrap_needed: bool,
    fields: Vec<ClassField>,
}

struct ClassField {
    comments: Vec<String>,
    name: String,
    readonly: bool,
}

pub struct SubContext<'a, 'b: 'a> {
    pub program: &'a shared::Program,
    pub cx: &'a mut Context<'b>,
}

const INITIAL_SLAB_VALUES: &[&str] = &["undefined", "null", "true", "false"];

impl<'a> Context<'a> {
    fn export(&mut self, name: &str, contents: &str, comments: Option<String>) {
        let contents = contents.trim();
        if let Some(ref c) = comments {
            self.globals.push_str(c);
        }
        let global = if self.use_node_require() {
            if contents.starts_with("class") {
                format!("{1}\nmodule.exports.{0} = {0};\n", name, contents)
            } else {
                format!("module.exports.{} = {};\n", name, contents)
            }
        } else if self.config.no_modules {
            if contents.starts_with("class") {
                format!("{1}\n__exports.{0} = {0};\n", name, contents)
            } else {
                format!("__exports.{} = {};\n", name, contents)
            }
        } else {
            if contents.starts_with("function") {
                format!("export function {}{}\n", name, &contents[8..])
            } else if contents.starts_with("class") {
                format!("export {}\n", contents)
            } else {
                format!("export const {} = {};\n", name, contents)
            }
        };
        self.global(&global);
    }

    fn require_internal_export(&mut self, name: &'static str) -> Result<(), Error> {
        if !self.required_internal_exports.insert(name) {
            return Ok(());
        }
        if let Some(s) = self.module.export_section() {
            if s.entries().iter().any(|e| e.field() == name) {
                return Ok(());
            }
        }

        bail!(
            "the exported function `{}` is required to generate bindings \
             but it was not found in the wasm file, perhaps the `std` feature \
             of the `wasm-bindgen` crate needs to be enabled?",
            name
        );
    }

    pub fn finalize(&mut self, module_name: &str) -> Result<(String, String), Error> {
        self.write_classes()?;

        self.bind("__wbindgen_object_clone_ref", &|me| {
            me.expose_add_heap_object();
            me.expose_get_object();
            let bump_cnt = if me.config.debug {
                String::from(
                    "
                    if (typeof(val) === 'number') throw new Error('corrupt slab');
                    val.cnt += 1;
                    ",
                )
            } else {
                String::from("val.cnt += 1;")
            };
            Ok(format!(
                "
                function(idx) {{
                    // If this object is on the stack promote it to the heap.
                    if ((idx & 1) === 1) return addHeapObject(getObject(idx));

                    // Otherwise if the object is on the heap just bump the
                    // refcount and move on
                    const val = slab[idx >> 1];
                    {}
                    return idx;
                }}
                ",
                bump_cnt
            ))
        })?;

        self.bind("__wbindgen_object_drop_ref", &|me| {
            me.expose_drop_ref();
            Ok(String::from(
                "
                function(i) {
                    dropRef(i);
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_string_new", &|me| {
            me.expose_add_heap_object();
            me.expose_get_string_from_wasm();
            Ok(String::from(
                "
                function(p, l) {
                    return addHeapObject(getStringFromWasm(p, l));
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_number_new", &|me| {
            me.expose_add_heap_object();
            Ok(String::from(
                "
                function(i) {
                    return addHeapObject(i);
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_number_get", &|me| {
            me.expose_get_object();
            me.expose_uint8_memory();
            Ok(String::from(
                "
                function(n, invalid) {
                    let obj = getObject(n);
                    if (typeof(obj) === 'number') return obj;
                    getUint8Memory()[invalid] = 1;
                    return 0;
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_is_null", &|me| {
            me.expose_get_object();
            Ok(String::from(
                "
                function(idx) {
                    return getObject(idx) === null ? 1 : 0;
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_is_undefined", &|me| {
            me.expose_get_object();
            Ok(String::from(
                "
                function(idx) {
                    return getObject(idx) === undefined ? 1 : 0;
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_boolean_get", &|me| {
            me.expose_get_object();
            Ok(String::from(
                "
                function(i) {
                    let v = getObject(i);
                    if (typeof(v) === 'boolean') {
                        return v ? 1 : 0;
                    } else {
                        return 2;
                    }
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_symbol_new", &|me| {
            me.expose_get_string_from_wasm();
            me.expose_add_heap_object();
            Ok(String::from(
                "
                function(ptr, len) {
                    let a;
                    if (ptr === 0) {
                        a = Symbol();
                    } else {
                        a = Symbol(getStringFromWasm(ptr, len));
                    }
                    return addHeapObject(a);
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_is_symbol", &|me| {
            me.expose_get_object();
            Ok(String::from(
                "
                function(i) {
                    return typeof(getObject(i)) === 'symbol' ? 1 : 0;
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_is_object", &|me| {
            me.expose_get_object();
            Ok(String::from(
                "
                function(i) {
                    const val = getObject(i);
                    return typeof(val) === 'object' && val !== null ? 1 : 0;
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_is_function", &|me| {
            me.expose_get_object();
            Ok(String::from(
                "
                function(i) {
                    return typeof(getObject(i)) === 'function' ? 1 : 0;
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_is_string", &|me| {
            me.expose_get_object();
            Ok(String::from(
                "
                function(i) {
                    return typeof(getObject(i)) === 'string' ? 1 : 0;
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_string_get", &|me| {
            me.expose_pass_string_to_wasm()?;
            me.expose_get_object();
            me.expose_uint32_memory();
            Ok(String::from(
                "
                function(i, len_ptr) {
                    let obj = getObject(i);
                    if (typeof(obj) !== 'string') return 0;
                    const [ptr, len] = passStringToWasm(obj);
                    getUint32Memory()[len_ptr / 4] = len;
                    return ptr;
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_cb_drop", &|me| {
            me.expose_drop_ref();
            Ok(String::from(
                "
                function(i) {
                    let obj = getObject(i).original;
                    obj.a = obj.b = 0;
                    dropRef(i);
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_cb_forget", &|me| {
            me.expose_drop_ref();
            Ok(String::from(
                "
                function(i) {
                    dropRef(i);
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_json_parse", &|me| {
            me.expose_add_heap_object();
            me.expose_get_string_from_wasm();
            Ok(String::from(
                "
                function(ptr, len) {
                    return addHeapObject(JSON.parse(getStringFromWasm(ptr, len)));
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_json_serialize", &|me| {
            me.expose_get_object();
            me.expose_pass_string_to_wasm()?;
            me.expose_uint32_memory();
            Ok(String::from(
                "
                function(idx, ptrptr) {
                    const [ptr, len] = passStringToWasm(JSON.stringify(getObject(idx)));
                    getUint32Memory()[ptrptr / 4] = ptr;
                    return len;
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_jsval_eq", &|me| {
            me.expose_get_object();
            Ok(String::from(
                "
                function(a, b) {
                    return getObject(a) === getObject(b) ? 1 : 0;
                }
                ",
            ))
        })?;

        self.bind("__wbindgen_memory", &|me| {
            me.expose_add_heap_object();
            let mem = me.memory();
            Ok(format!(
                "
                function() {{
                    return addHeapObject({});
                }}
                ", mem
            ))
        })?;

        self.bind("__wbindgen_rethrow", &|me| {
            me.expose_take_object();
            Ok(String::from("function(idx) { throw takeObject(idx); }"))
        })?;

        self.create_memory_export();
        self.unexport_unused_internal_exports();
        closures::rewrite(self)?;
        self.gc()?;

        // Note that it's important `throw` comes last *after* we gc. The
        // `__wbindgen_malloc` function may call this but we only want to
        // generate code for this if it's actually live (and __wbindgen_malloc
        // isn't gc'd).
        self.bind("__wbindgen_throw", &|me| {
            me.expose_get_string_from_wasm();
            Ok(String::from(
                "
                function(ptr, len) {
                    throw new Error(getStringFromWasm(ptr, len));
                }
                ",
            ))
        })?;

        self.rewrite_imports(module_name);

        let mut js = if self.config.no_modules {
            format!(
                    "\
                    (function() {{
                        var wasm;
                        const __exports = {{}};
                        {globals}
                        function init(wasm_path) {{
                            const fetchPromise = fetch(wasm_path);
                            let resultPromise;
                            if (typeof WebAssembly.instantiateStreaming === 'function') {{
                                resultPromise = WebAssembly.instantiateStreaming(fetchPromise, {{ './{module}': __exports }});
                            }} else {{
                                resultPromise = fetchPromise
                                    .then(response => response.arrayBuffer())
                                    .then(buffer => WebAssembly.instantiate(buffer, {{ './{module}': __exports }}));
                            }}
                            return resultPromise.then(({{instance}}) => {{
                                wasm = init.wasm = instance.exports;
                                return;
                            }});
                        }};
                        self.{global_name} = Object.assign(init, __exports);
                    }})();",
                    globals = self.globals,
                    module = module_name,
                    global_name = self.config.no_modules_global
                        .as_ref()
                        .map(|s| &**s)
                        .unwrap_or("wasm_bindgen"),
            )
        } else {
            let import_wasm = if self.globals.len() == 0 {
                String::new()
            } else if self.use_node_require() {
                self.footer
                    .push_str(&format!("wasm = require('./{}_bg');", module_name));
                format!("var wasm;")
            } else {
                format!("import * as wasm from './{}_bg';", module_name)
            };

            format!(
                "\
                /* tslint:disable */\n\
                {import_wasm}\n\
                {imports}\n\

                {globals}\n\
                {footer}",
                import_wasm = import_wasm,
                globals = self.globals,
                imports = self.imports,
                footer = self.footer,
            )
        };

        self.export_table();
        self.gc()?;

        while js.contains("\n\n\n") {
            js = js.replace("\n\n\n", "\n\n");
        }

        Ok((js, self.typescript.clone()))
    }

    fn bind(
        &mut self,
        name: &str,
        f: &Fn(&mut Self) -> Result<String, Error>,
    ) -> Result<(), Error> {
        if !self.wasm_import_needed(name) {
            return Ok(());
        }
        let contents = f(self)
            .with_context(|_| format!("failed to generate internal JS function `{}`", name))?;
        self.export(name, &contents, None);
        Ok(())
    }

    fn write_classes(&mut self) -> Result<(), Error> {
        let classes = mem::replace(&mut self.exported_classes, Default::default());
        for (class, exports) in classes {
            self.write_class(&class, &exports)?;
        }
        Ok(())
    }

    fn write_class(&mut self, name: &str, class: &ExportedClass) -> Result<(), Error> {
        let mut dst = format!("class {} {{\n", name);
        let mut ts_dst = format!("export {}", dst);

        let (mkweakref, freeref) = if self.config.weak_refs {
            // When weak refs are enabled we use them to automatically free the
            // contents of an exported rust class when it's gc'd. Note that a
            // manual `free` function still exists for deterministic
            // destruction.
            //
            // This is implemented by using a `WeakRefGroup` to run finalizers
            // for all `WeakRef` objects that it creates. Upon construction of
            // a new wasm object we use `makeRef` with "holdings" of a thunk to
            // free the wasm instance.  Once the `this` (the instance we're
            // creating) is gc'd then the finalizer will run with the
            // `WeakRef`, and we'll pull out the `holdings`, our pointer.
            //
            // Note, though, that if manual finalization happens we want to
            // cancel the `WeakRef`-generated finalization, so we retain the
            // `WeakRef` in a global map. This global map is then used to
            // `drop()` the `WeakRef` (cancel finalization) whenever it is
            // finalized.
            self.expose_cleanup_groups();
            let mk = format!("addCleanup(this, this.ptr, free{});", name);
            let free = "
                CLEANUPS_MAP.get(ptr).drop();
                CLEANUPS_MAP.delete(ptr);
            ";
            (mk, free)
        } else {
            (String::new(), "")
        };

        if self.config.debug && !class.has_constructor {
            dst.push_str(
                "
                    constructor() {
                        throw new Error('cannot invoke `new` directly');
                    }
                "
            );
        }

        let mut wrap_needed = class.wrap_needed;
        let new_name = shared::new_function(&name);
        if self.wasm_import_needed(&new_name) {
            self.expose_add_heap_object();
            wrap_needed = true;

            self.export(
                &new_name,
                &format!(
                    "
                    function(ptr) {{
                        return addHeapObject({}.__wrap(ptr));
                    }}
                    ",
                    name
                ),
                None,
            );
        }

        if wrap_needed {
            dst.push_str(&format!(
                "
                static __wrap(ptr) {{
                    const obj = Object.create({}.prototype);
                    obj.ptr = ptr;
                    {}
                    return obj;
                }}
                ",
                name,
                mkweakref.replace("this", "obj"),
            ));
        }

        for field in class.fields.iter() {
            let wasm_getter = shared::struct_field_get(name, &field.name);
            let wasm_setter = shared::struct_field_set(name, &field.name);
            let descriptor = match self.describe(&wasm_getter) {
                None => continue,
                Some(d) => d,
            };

            let set = {
                let mut cx = Js2Rust::new(&field.name, self);
                cx.method(true, false)
                    .argument(&descriptor)?
                    .ret(&Descriptor::Unit)?;
                ts_dst.push_str(&format!(
                    "{}{}: {}\n",
                    if field.readonly { "readonly " } else { "" },
                    field.name,
                    &cx.js_arguments[0].1
                ));
                cx.finish("", &format!("wasm.{}", wasm_setter)).0
            };
            let (get, _ts, js_doc) = Js2Rust::new(&field.name, self)
                .method(true, false)
                .ret(&descriptor)?
                .finish("", &format!("wasm.{}", wasm_getter));
            if !dst.ends_with("\n") {
                dst.push_str("\n");
            }
            dst.push_str(&format_doc_comments(&field.comments, Some(js_doc)));
            dst.push_str("get ");
            dst.push_str(&field.name);
            dst.push_str(&get);
            dst.push_str("\n");
            if !field.readonly {
                dst.push_str("set ");
                dst.push_str(&field.name);
                dst.push_str(&set);
            }
        }

        self.global(&format!(
            "
            function free{}(ptr) {{
                {}
                wasm.{}(ptr);
            }}
            ",
            name,
            freeref,
            shared::free_function(&name)
        ));
        dst.push_str(&format!(
            "
            free() {{
                const ptr = this.ptr;
                this.ptr = 0;
                free{}(ptr);
            }}
            ",
            name,
        ));
        ts_dst.push_str("free(): void;\n");
        dst.push_str(&class.contents);
        ts_dst.push_str(&class.typescript);
        dst.push_str("}\n");
        ts_dst.push_str("}\n");

        self.export(&name, &dst, Some(class.comments.clone()));
        self.typescript.push_str(&ts_dst);

        Ok(())
    }

    fn export_table(&mut self) {
        if !self.function_table_needed {
            return;
        }
        for section in self.module.sections_mut() {
            let exports = match *section {
                Section::Export(ref mut s) => s,
                _ => continue,
            };
            let entry = ExportEntry::new("__wbg_function_table".to_string(), Internal::Table(0));
            exports.entries_mut().push(entry);
            break;
        }
    }

    fn create_memory_export(&mut self) {
        let limits = match self.memory_init.clone() {
            Some(limits) => limits,
            None => return,
        };
        let mut initializer = String::from("new WebAssembly.Memory({");
        initializer.push_str(&format!("initial:{}", limits.initial()));
        if let Some(max) = limits.maximum() {
            initializer.push_str(&format!(",maximum:{}", max));
        }
        initializer.push_str("})");
        self.export("memory", &initializer, None);
    }

    fn rewrite_imports(&mut self, module_name: &str) {
        for (name, contents) in self._rewrite_imports(module_name) {
            self.export(&name, &contents, None);
        }
    }

    fn _rewrite_imports(&mut self, module_name: &str) -> Vec<(String, String)> {
        let mut math_imports = Vec::new();
        let imports = self
            .module
            .sections_mut()
            .iter_mut()
            .filter_map(|s| match *s {
                Section::Import(ref mut s) => Some(s),
                _ => None,
            })
            .flat_map(|s| s.entries_mut());

        for import in imports {
            if import.module() == "__wbindgen_placeholder__" {
                import.module_mut().truncate(0);
                import.module_mut().push_str("./");
                import.module_mut().push_str(module_name);
                continue;
            }

            if import.module() != "env" {
                continue;
            }

            // If memory is imported we'll have exported it from the shim module
            // so let's import it from there.
            if import.field() == "memory" {
                import.module_mut().truncate(0);
                import.module_mut().push_str("./");
                import.module_mut().push_str(module_name);
                continue
            }

            let renamed_import = format!("__wbindgen_{}", import.field());
            let mut bind_math = |expr: &str| {
                math_imports.push((renamed_import.clone(), format!("function{}", expr)));
            };

            // FIXME(#32): try to not use function shims
            match import.field() {
                "Math_acos" => bind_math("(x) { return Math.acos(x); }"),
                "Math_asin" => bind_math("(x) { return Math.asin(x); }"),
                "Math_atan" => bind_math("(x) { return Math.atan(x); }"),
                "Math_atan2" => bind_math("(x, y) { return Math.atan2(x, y); }"),
                "Math_cbrt" => bind_math("(x) { return Math.cbrt(x); }"),
                "Math_cosh" => bind_math("(x) { return Math.cosh(x); }"),
                "Math_expm1" => bind_math("(x) { return Math.expm1(x); }"),
                "Math_hypot" => bind_math("(x, y) { return Math.hypot(x, y); }"),
                "Math_log1p" => bind_math("(x) { return Math.log1p(x); }"),
                "Math_sinh" => bind_math("(x) { return Math.sinh(x); }"),
                "Math_tan" => bind_math("(x) { return Math.tan(x); }"),
                "Math_tanh" => bind_math("(x) { return Math.tanh(x); }"),
                _ => continue,
            }

            import.module_mut().truncate(0);
            import.module_mut().push_str("./");
            import.module_mut().push_str(module_name);
            *import.field_mut() = renamed_import.clone();
        }

        math_imports
    }

    fn unexport_unused_internal_exports(&mut self) {
        let required = &self.required_internal_exports;
        for section in self.module.sections_mut() {
            let exports = match *section {
                Section::Export(ref mut s) => s,
                _ => continue,
            };
            exports.entries_mut().retain(|export| {
                !export.field().starts_with("__wbindgen") || required.contains(export.field())
            });
        }
    }

    fn expose_drop_ref(&mut self) {
        if !self.exposed_globals.insert("drop_ref") {
            return;
        }
        self.expose_global_slab();
        self.expose_global_slab_next();
        let validate_owned = if self.config.debug {
            String::from(
                "
                if ((idx & 1) === 1) throw new Error('cannot drop ref of stack objects');
                ",
            )
        } else {
            String::new()
        };
        let dec_ref = if self.config.debug {
            String::from(
                "
                if (typeof(obj) === 'number') throw new Error('corrupt slab');
                obj.cnt -= 1;
                if (obj.cnt > 0) return;
                ",
            )
        } else {
            String::from(
                "
                obj.cnt -= 1;
                if (obj.cnt > 0) return;
                ",
            )
        };
        self.global(&format!(
            "
            function dropRef(idx) {{
                {}
                idx = idx >> 1;
                if (idx < {}) return;
                let obj = slab[idx];
                {}
                // If we hit 0 then free up our space in the slab
                slab[idx] = slab_next;
                slab_next = idx;
            }}
            ",
            validate_owned, INITIAL_SLAB_VALUES.len(), dec_ref
        ));
    }

    fn expose_global_stack(&mut self) {
        if !self.exposed_globals.insert("stack") {
            return;
        }
        self.global(&format!(
            "
            const stack = [];
        "
        ));
        if self.config.debug {
            self.export(
                "assertStackEmpty",
                "
                function() {
                    if (stack.length === 0) return;
                    throw new Error('stack is not currently empty');
                }
                ",
                None,
            );
        }
    }

    fn expose_global_slab(&mut self) {
        if !self.exposed_globals.insert("slab") {
            return;
        }
        let initial_values = INITIAL_SLAB_VALUES.iter()
            .map(|s| format!("{{ obj: {} }}", s))
            .collect::<Vec<_>>();
        self.global(&format!("const slab = [{}];", initial_values.join(", ")));
        if self.config.debug {
            self.export(
                "assertSlabEmpty",
                &format!(
                    "
                    function() {{
                        for (let i = {}; i < slab.length; i++) {{
                            if (typeof(slab[i]) === 'number') continue;
                            throw new Error('slab is not currently empty');
                        }}
                    }}
                    ",
                    initial_values.len()
                ),
                None,
            );
        }
    }

    fn expose_global_slab_next(&mut self) {
        if !self.exposed_globals.insert("slab_next") {
            return;
        }
        self.expose_global_slab();
        self.global(
            "
            let slab_next = slab.length;
            ",
        );
    }

    fn expose_get_object(&mut self) {
        if !self.exposed_globals.insert("get_object") {
            return;
        }
        self.expose_global_stack();
        self.expose_global_slab();

        let get_obj = if self.config.debug {
            String::from(
                "
                if (typeof(val) === 'number') throw new Error('corrupt slab');
                return val.obj;
                ",
            )
        } else {
            String::from(
                "
                return val.obj;
                ",
            )
        };
        self.global(&format!(
            "
            function getObject(idx) {{
                if ((idx & 1) === 1) {{
                    return stack[idx >> 1];
                }} else {{
                    const val = slab[idx >> 1];
                    {}
                }}
            }}
            ",
            get_obj
        ));
    }

    fn expose_assert_num(&mut self) {
        if !self.exposed_globals.insert("assert_num") {
            return;
        }
        self.global(&format!(
            "
            function _assertNum(n) {{
                if (typeof(n) !== 'number') throw new Error('expected a number argument');
            }}
            "
        ));
    }

    fn expose_assert_bool(&mut self) {
        if !self.exposed_globals.insert("assert_bool") {
            return;
        }
        self.global(&format!(
            "
            function _assertBoolean(n) {{
                if (typeof(n) !== 'boolean') {{
                    throw new Error('expected a boolean argument');
                }}
            }}
            "
        ));
    }

    fn expose_pass_string_to_wasm(&mut self) -> Result<(), Error> {
        if !self.exposed_globals.insert("pass_string_to_wasm") {
            return Ok(());
        }
        self.require_internal_export("__wbindgen_malloc")?;
        self.expose_text_encoder();
        self.expose_uint8_memory();
        let debug = if self.config.debug {
            "
                if (typeof(arg) !== 'string') throw new Error('expected a string argument');
            "
        } else {
            ""
        };
        self.global(&format!(
            "
            function passStringToWasm(arg) {{
                {}
                const buf = cachedEncoder.encode(arg);
                const ptr = wasm.__wbindgen_malloc(buf.length);
                getUint8Memory().set(buf, ptr);
                return [ptr, buf.length];
            }}
            ",
            debug
        ));
        Ok(())
    }

    fn expose_pass_array8_to_wasm(&mut self) -> Result<(), Error> {
        self.expose_uint8_memory();
        self.pass_array_to_wasm("passArray8ToWasm", "getUint8Memory", 1)
    }

    fn expose_pass_array16_to_wasm(&mut self) -> Result<(), Error> {
        self.expose_uint16_memory();
        self.pass_array_to_wasm("passArray16ToWasm", "getUint16Memory", 2)
    }

    fn expose_pass_array32_to_wasm(&mut self) -> Result<(), Error> {
        self.expose_uint32_memory();
        self.pass_array_to_wasm("passArray32ToWasm", "getUint32Memory", 4)
    }

    fn expose_pass_array64_to_wasm(&mut self) -> Result<(), Error> {
        self.expose_uint64_memory();
        self.pass_array_to_wasm("passArray64ToWasm", "getUint64Memory", 8)
    }

    fn expose_pass_array_f32_to_wasm(&mut self) -> Result<(), Error> {
        self.expose_f32_memory();
        self.pass_array_to_wasm("passArrayF32ToWasm", "getFloat32Memory", 4)
    }

    fn expose_pass_array_f64_to_wasm(&mut self) -> Result<(), Error> {
        self.expose_f64_memory();
        self.pass_array_to_wasm("passArrayF64ToWasm", "getFloat64Memory", 8)
    }

    fn expose_pass_array_jsvalue_to_wasm(&mut self) -> Result<(), Error> {
        if !self.exposed_globals.insert("pass_array_jsvalue") {
            return Ok(());
        }
        self.require_internal_export("__wbindgen_malloc")?;
        self.expose_uint32_memory();
        self.expose_add_heap_object();
        self.global("
            function passArrayJsValueToWasm(array) {
                const ptr = wasm.__wbindgen_malloc(array.length * 4);
                const mem = getUint32Memory();
                for (let i = 0; i < array.length; i++) {
                    mem[ptr / 4 + i] = addHeapObject(array[i]);
                }
                return [ptr, array.length];
            }

        ");
        Ok(())
    }

    fn pass_array_to_wasm(
        &mut self,
        name: &'static str,
        delegate: &str,
        size: usize,
    ) -> Result<(), Error> {
        if !self.exposed_globals.insert(name) {
            return Ok(());
        }
        self.require_internal_export("__wbindgen_malloc")?;
        self.global(&format!(
            "
            function {}(arg) {{
                const ptr = wasm.__wbindgen_malloc(arg.length * {size});
                {}().set(arg, ptr / {size});
                return [ptr, arg.length];
            }}
            ",
            name,
            delegate,
            size = size
        ));
        Ok(())
    }

    fn expose_text_encoder(&mut self) {
        if !self.exposed_globals.insert("text_encoder") {
            return;
        }
        if self.config.nodejs_experimental_modules {
            self.imports
                .push_str("import { TextEncoder } from 'util';\n");
        } else if self.config.nodejs {
            self.global(
                "
                const TextEncoder = require('util').TextEncoder;
                ",
            );
        } else if !(self.config.browser || self.config.no_modules) {
            self.global(
                "
                const TextEncoder = typeof self === 'object' && self.TextEncoder
                    ? self.TextEncoder
                    : require('util').TextEncoder;
                ",
            );
        }
        self.global(
            "
            let cachedEncoder = new TextEncoder('utf-8');
            ",
        );
    }

    fn expose_text_decoder(&mut self) {
        if !self.exposed_globals.insert("text_decoder") {
            return;
        }
        if self.config.nodejs_experimental_modules {
            self.imports
                .push_str("import { TextDecoder } from 'util';\n");
        } else if self.config.nodejs {
            self.global(
                "
                const TextDecoder = require('util').TextDecoder;
                ",
            );
        } else if !(self.config.browser || self.config.no_modules) {
            self.global(
                "
                const TextDecoder = typeof self === 'object' && self.TextDecoder
                    ? self.TextDecoder
                    : require('util').TextDecoder;
                ",
            );
        }
        self.global(
            "
            let cachedDecoder = new TextDecoder('utf-8');
            ",
        );
    }

    fn expose_get_string_from_wasm(&mut self) {
        if !self.exposed_globals.insert("get_string_from_wasm") {
            return;
        }
        self.expose_text_decoder();
        self.expose_uint8_memory();

        // Typically we try to give a raw view of memory out to `TextDecoder` to
        // avoid copying too much data. If, however, a `SharedArrayBuffer` is
        // being used it looks like that is rejected by `TextDecoder` or
        // otherwise doesn't work with it. When we detect a shared situation we
        // use `slice` which creates a new array instead of `subarray` which
        // creates just a view. That way in shared mode we copy more data but in
        // non-shared mode there's no need to copy the data except for the
        // string itself.
        self.memory(); // set self.memory_init
        let is_shared = self.module
            .memory_section()
            .map(|s| s.entries()[0].limits().shared())
            .unwrap_or(match &self.memory_init {
                Some(limits) => limits.shared(),
                None => false,
            });
        let method = if is_shared { "slice" } else { "subarray" };

        self.global(&format!("
            function getStringFromWasm(ptr, len) {{
                return cachedDecoder.decode(getUint8Memory().{}(ptr, ptr + len));
            }}
        ", method));
    }

    fn expose_get_array_js_value_from_wasm(&mut self) {
        if !self.exposed_globals.insert("get_array_js_value_from_wasm") {
            return;
        }
        self.expose_uint32_memory();
        self.expose_take_object();
        self.global(
            "
            function getArrayJsValueFromWasm(ptr, len) {
                const mem = getUint32Memory();
                const slice = mem.subarray(ptr / 4, ptr / 4 + len);
                const result = [];
                for (let i = 0; i < slice.length; i++) {
                    result.push(takeObject(slice[i]));
                }
                return result;
            }
            ",
        );
    }

    fn expose_get_array_i8_from_wasm(&mut self) {
        self.expose_int8_memory();
        self.arrayget("getArrayI8FromWasm", "getInt8Memory", 1);
    }

    fn expose_get_array_u8_from_wasm(&mut self) {
        self.expose_uint8_memory();
        self.arrayget("getArrayU8FromWasm", "getUint8Memory", 1);
    }

    fn expose_get_clamped_array_u8_from_wasm(&mut self) {
        self.expose_clamped_uint8_memory();
        self.arrayget("getClampedArrayU8FromWasm", "getUint8ClampedMemory", 1);
    }

    fn expose_get_array_i16_from_wasm(&mut self) {
        self.expose_int16_memory();
        self.arrayget("getArrayI16FromWasm", "getInt16Memory", 2);
    }

    fn expose_get_array_u16_from_wasm(&mut self) {
        self.expose_uint16_memory();
        self.arrayget("getArrayU16FromWasm", "getUint16Memory", 2);
    }

    fn expose_get_array_i32_from_wasm(&mut self) {
        self.expose_int32_memory();
        self.arrayget("getArrayI32FromWasm", "getInt32Memory", 4);
    }

    fn expose_get_array_u32_from_wasm(&mut self) {
        self.expose_uint32_memory();
        self.arrayget("getArrayU32FromWasm", "getUint32Memory", 4);
    }

    fn expose_get_array_i64_from_wasm(&mut self) {
        self.expose_int64_memory();
        self.arrayget("getArrayI64FromWasm", "getInt64Memory", 8);
    }

    fn expose_get_array_u64_from_wasm(&mut self) {
        self.expose_uint64_memory();
        self.arrayget("getArrayU64FromWasm", "getUint64Memory", 8);
    }

    fn expose_get_array_f32_from_wasm(&mut self) {
        self.expose_f32_memory();
        self.arrayget("getArrayF32FromWasm", "getFloat32Memory", 4);
    }

    fn expose_get_array_f64_from_wasm(&mut self) {
        self.expose_f64_memory();
        self.arrayget("getArrayF64FromWasm", "getFloat64Memory", 8);
    }

    fn arrayget(&mut self, name: &'static str, mem: &'static str, size: usize) {
        if !self.exposed_globals.insert(name) {
            return;
        }
        self.global(&format!(
            "
            function {name}(ptr, len) {{
                return {mem}().subarray(ptr / {size}, ptr / {size} + len);
            }}
            ",
            name = name,
            mem = mem,
            size = size,
        ));
    }

    fn expose_int8_memory(&mut self) {
        self.memview("getInt8Memory", "Int8Array");
    }

    fn expose_uint8_memory(&mut self) {
        self.memview("getUint8Memory", "Uint8Array");
    }

    fn expose_clamped_uint8_memory(&mut self) {
        self.memview("getUint8ClampedMemory", "Uint8ClampedArray");
    }

    fn expose_int16_memory(&mut self) {
        self.memview("getInt16Memory", "Int16Array");
    }

    fn expose_uint16_memory(&mut self) {
        self.memview("getUint16Memory", "Uint16Array");
    }

    fn expose_int32_memory(&mut self) {
        self.memview("getInt32Memory", "Int32Array");
    }

    fn expose_uint32_memory(&mut self) {
        self.memview("getUint32Memory", "Uint32Array");
    }

    fn expose_int64_memory(&mut self) {
        self.memview("getInt64Memory", "BigInt64Array");
    }

    fn expose_uint64_memory(&mut self) {
        self.memview("getUint64Memory", "BigUint64Array");
    }

    fn expose_f32_memory(&mut self) {
        self.memview("getFloat32Memory", "Float32Array");
    }

    fn expose_f64_memory(&mut self) {
        self.memview("getFloat64Memory", "Float64Array");
    }

    fn memview_function(&mut self, t: VectorKind) -> &'static str {
        match t {
            VectorKind::String => {
                self.expose_uint8_memory();
                "getUint8Memory"
            }
            VectorKind::I8 => {
                self.expose_int8_memory();
                "getInt8Memory"
            }
            VectorKind::U8 => {
                self.expose_uint8_memory();
                "getUint8Memory"
            }
            VectorKind::ClampedU8 => {
                self.expose_clamped_uint8_memory();
                "getUint8ClampedMemory"
            }
            VectorKind::I16 => {
                self.expose_int16_memory();
                "getInt16Memory"
            }
            VectorKind::U16 => {
                self.expose_uint16_memory();
                "getUint16Memory"
            }
            VectorKind::I32 => {
                self.expose_int32_memory();
                "getInt32Memory"
            }
            VectorKind::U32 => {
                self.expose_uint32_memory();
                "getUint32Memory"
            }
            VectorKind::I64 => {
                self.expose_int64_memory();
                "getInt64Memory"
            }
            VectorKind::U64 => {
                self.expose_uint64_memory();
                "getUint64Memory"
            }
            VectorKind::F32 => {
                self.expose_f32_memory();
                "getFloat32Memory"
            }
            VectorKind::F64 => {
                self.expose_f64_memory();
                "getFloat64Memory"
            }
            VectorKind::Anyref => {
                self.expose_uint32_memory();
                "getUint32Memory"
            }
        }
    }

    fn memview(&mut self, name: &'static str, js: &str) {
        if !self.exposed_globals.insert(name) {
            return;
        }
        let mem = self.memory();
        self.global(&format!(
            "
            let cache{name} = null;
            function {name}() {{
                if (cache{name} === null || cache{name}.buffer !== {mem}.buffer) {{
                    cache{name} = new {js}({mem}.buffer);
                }}
                return cache{name};
            }}
            ",
            name = name,
            js = js,
            mem = mem,
        ));
    }

    fn expose_assert_class(&mut self) {
        if !self.exposed_globals.insert("assert_class") {
            return;
        }
        self.global(
            "
            function _assertClass(instance, klass) {
                if (!(instance instanceof klass)) {
                    throw new Error(`expected instance of ${klass.name}`);
                }
                return instance.ptr;
            }
            ",
        );
    }

    fn expose_borrowed_objects(&mut self) {
        if !self.exposed_globals.insert("borrowed_objects") {
            return;
        }
        self.expose_global_stack();
        self.global(
            "
            function addBorrowedObject(obj) {
                stack.push(obj);
                return ((stack.length - 1) << 1) | 1;
            }
            ",
        );
    }

    fn expose_take_object(&mut self) {
        if !self.exposed_globals.insert("take_object") {
            return;
        }
        self.expose_get_object();
        self.expose_drop_ref();
        self.global(
            "
            function takeObject(idx) {
                const ret = getObject(idx);
                dropRef(idx);
                return ret;
            }
            ",
        );
    }

    fn expose_add_heap_object(&mut self) {
        if !self.exposed_globals.insert("add_heap_object") {
            return;
        }
        self.expose_global_slab();
        self.expose_global_slab_next();
        let set_slab_next = if self.config.debug {
            String::from(
                "
                if (typeof(next) !== 'number') throw new Error('corrupt slab');
                slab_next = next;
                ",
            )
        } else {
            String::from(
                "
                slab_next = next;
                ",
            )
        };
        self.global(&format!(
            "
            function addHeapObject(obj) {{
                if (slab_next === slab.length) slab.push(slab.length + 1);
                const idx = slab_next;
                const next = slab[idx];
                {}
                slab[idx] = {{ obj, cnt: 1 }};
                return idx << 1;
            }}
            ",
            set_slab_next
        ));
    }

    fn wasm_import_needed(&self, name: &str) -> bool {
        let imports = match self.module.import_section() {
            Some(s) => s,
            None => return false,
        };

        imports
            .entries()
            .iter()
            .any(|i| i.module() == "__wbindgen_placeholder__" && i.field() == name)
    }

    fn pass_to_wasm_function(&mut self, t: VectorKind) -> Result<&'static str, Error> {
        let s = match t {
            VectorKind::String => {
                self.expose_pass_string_to_wasm()?;
                "passStringToWasm"
            }
            VectorKind::I8 | VectorKind::U8 | VectorKind::ClampedU8 => {
                self.expose_pass_array8_to_wasm()?;
                "passArray8ToWasm"
            }
            VectorKind::U16 | VectorKind::I16 => {
                self.expose_pass_array16_to_wasm()?;
                "passArray16ToWasm"
            }
            VectorKind::I32 | VectorKind::U32 => {
                self.expose_pass_array32_to_wasm()?;
                "passArray32ToWasm"
            }
            VectorKind::I64 | VectorKind::U64 => {
                self.expose_pass_array64_to_wasm()?;
                "passArray64ToWasm"
            }
            VectorKind::F32 => {
                self.expose_pass_array_f32_to_wasm()?;
                "passArrayF32ToWasm"
            }
            VectorKind::F64 => {
                self.expose_pass_array_f64_to_wasm()?;
                "passArrayF64ToWasm"
            }
            VectorKind::Anyref => {
                self.expose_pass_array_jsvalue_to_wasm()?;
                "passArrayJsValueToWasm"
            }
        };
        Ok(s)
    }

    fn expose_get_vector_from_wasm(&mut self, ty: VectorKind) -> &'static str {
        match ty {
            VectorKind::String => {
                self.expose_get_string_from_wasm();
                "getStringFromWasm"
            }
            VectorKind::I8 => {
                self.expose_get_array_i8_from_wasm();
                "getArrayI8FromWasm"
            }
            VectorKind::U8 => {
                self.expose_get_array_u8_from_wasm();
                "getArrayU8FromWasm"
            }
            VectorKind::ClampedU8 => {
                self.expose_get_clamped_array_u8_from_wasm();
                "getClampedArrayU8FromWasm"
            }
            VectorKind::I16 => {
                self.expose_get_array_i16_from_wasm();
                "getArrayI16FromWasm"
            }
            VectorKind::U16 => {
                self.expose_get_array_u16_from_wasm();
                "getArrayU16FromWasm"
            }
            VectorKind::I32 => {
                self.expose_get_array_i32_from_wasm();
                "getArrayI32FromWasm"
            }
            VectorKind::U32 => {
                self.expose_get_array_u32_from_wasm();
                "getArrayU32FromWasm"
            }
            VectorKind::I64 => {
                self.expose_get_array_i64_from_wasm();
                "getArrayI64FromWasm"
            }
            VectorKind::U64 => {
                self.expose_get_array_u64_from_wasm();
                "getArrayU64FromWasm"
            }
            VectorKind::F32 => {
                self.expose_get_array_f32_from_wasm();
                "getArrayF32FromWasm"
            }
            VectorKind::F64 => {
                self.expose_get_array_f64_from_wasm();
                "getArrayF64FromWasm"
            }
            VectorKind::Anyref => {
                self.expose_get_array_js_value_from_wasm();
                "getArrayJsValueFromWasm"
            }
        }
    }

    fn expose_get_global_argument(&mut self) -> Result<(), Error> {
        if !self.exposed_globals.insert("get_global_argument") {
            return Ok(());
        }
        self.expose_uint32_memory();
        self.expose_global_argument_ptr()?;
        self.global(
            "
            function getGlobalArgument(arg) {
                const idx = globalArgumentPtr() / 4 + arg;
                return getUint32Memory()[idx];
            }
            ",
        );
        Ok(())
    }

    fn expose_global_argument_ptr(&mut self) -> Result<(), Error> {
        if !self.exposed_globals.insert("global_argument_ptr") {
            return Ok(());
        }
        self.require_internal_export("__wbindgen_global_argument_ptr")?;
        self.global(
            "
            let cachedGlobalArgumentPtr = null;
            function globalArgumentPtr() {
                if (cachedGlobalArgumentPtr === null) {
                    cachedGlobalArgumentPtr = wasm.__wbindgen_global_argument_ptr();
                }
                return cachedGlobalArgumentPtr;
            }
            ",
        );
        Ok(())
    }

    fn expose_get_inherited_descriptor(&mut self) {
        if !self.exposed_globals.insert("get_inherited_descriptor") {
            return;
        }
        // It looks like while rare some browsers will move descriptors up the
        // property chain which runs the risk of breaking wasm-bindgen-generated
        // code because we're looking for precise descriptor functions rather
        // than relying on the prototype chain like most "normal JS" projects
        // do.
        //
        // As a result we have a small helper here which will walk the prototype
        // chain looking for a descriptor. For some more information on this see
        // #109
        self.global(
            "
            function GetOwnOrInheritedPropertyDescriptor(obj, id) {
              while (obj) {
                let desc = Object.getOwnPropertyDescriptor(obj, id);
                if (desc) return desc;
                obj = Object.getPrototypeOf(obj);
              }
              throw new Error(`descriptor for id='${id}' not found`);
            }
            ",
        );
    }

    fn expose_u32_cvt_shim(&mut self) -> &'static str {
        let name = "u32CvtShim";
        if !self.exposed_globals.insert(name) {
            return name;
        }
        self.global(&format!("const {} = new Uint32Array(2);", name));
        name
    }

    fn expose_int64_cvt_shim(&mut self) -> &'static str {
        let name = "int64CvtShim";
        if !self.exposed_globals.insert(name) {
            return name;
        }
        let n = self.expose_u32_cvt_shim();
        self.global(&format!(
            "const {} = new BigInt64Array({}.buffer);",
            name, n
        ));
        name
    }

    fn expose_uint64_cvt_shim(&mut self) -> &'static str {
        let name = "uint64CvtShim";
        if !self.exposed_globals.insert(name) {
            return name;
        }
        let n = self.expose_u32_cvt_shim();
        self.global(&format!(
            "const {} = new BigUint64Array({}.buffer);",
            name, n
        ));
        name
    }

    fn expose_is_like_none(&mut self) {
        if !self.exposed_globals.insert("is_like_none") {
            return
        }
        self.global("
            function isLikeNone(x) {
                return x === undefined || x === null;
            }
        ");
    }

    fn expose_cleanup_groups(&mut self) {
        if !self.exposed_globals.insert("cleanup_groups") {
            return
        }
        self.global(
            "
                const CLEANUPS = new WeakRefGroup(x => x.holdings());
                const CLEANUPS_MAP = new Map();

                function addCleanup(obj, ptr, free) {
                    const ref = CLEANUPS.makeRef(obj, () => free(ptr));
                    CLEANUPS_MAP.set(ptr, ref);
                }
            "
        );
    }

    fn gc(&mut self) -> Result<(), Error> {
        self.parse_wasm_names();
        let module = mem::replace(self.module, Module::default());
        let result = wasm_gc::Config::new()
            .demangle(self.config.demangle)
            .keep_debug(self.config.keep_debug || self.config.debug)
            .run(module, |m| parity_wasm::serialize(m).unwrap())?;
        *self.module = match result.into_module() {
            Ok(m) => m,
            Err(result) => deserialize_buffer(&result.into_bytes()?)?,
        };
        Ok(())
    }

    fn parse_wasm_names(&mut self) {
        let module = mem::replace(self.module, Module::default());
        let module = module.parse_names().unwrap_or_else(|p| p.1);
        *self.module = module;
    }

    fn describe(&mut self, name: &str) -> Option<Descriptor> {
        let name = format!("__wbindgen_describe_{}", name);
        let descriptor = self.interpreter.interpret_descriptor(&name, self.module)?;
        Some(Descriptor::decode(descriptor))
    }

    fn global(&mut self, s: &str) {
        let s = s.trim();

        // Ensure a blank line between adjacent items, and ensure everything is
        // terminated with a newline.
        while !self.globals.ends_with("\n\n\n") && !self.globals.ends_with("*/\n") {
            self.globals.push_str("\n");
        }
        self.globals.push_str(s);
        self.globals.push_str("\n");
    }

    fn use_node_require(&self) -> bool {
        self.config.nodejs && !self.config.nodejs_experimental_modules
    }

    fn memory(&mut self) -> &'static str {
        if self.module.memory_section().is_some() {
            return "wasm.memory";
        }

        let (entry, mem) = self.module.import_section()
            .expect("must import memory")
            .entries()
            .iter()
            .filter_map(|i| {
                match i.external() {
                    External::Memory(m) => Some((i, m)),
                    _ => None,
                }
            })
            .next()
            .expect("must import memory");
        assert_eq!(entry.module(), "env");
        assert_eq!(entry.field(), "memory");
        self.memory_init = Some(mem.limits().clone());
        "memory"
    }

    fn require_class_wrap(&mut self, class: &str) {
        self.exported_classes
            .entry(class.to_string())
            .or_insert_with(ExportedClass::default)
            .wrap_needed = true;
    }
}

impl<'a, 'b> SubContext<'a, 'b> {
    pub fn generate(&mut self) -> Result<(), Error> {
        for f in self.program.exports.iter() {
            self.generate_export(f).with_context(|_| {
                format!(
                    "failed to generate bindings for Rust export `{}`",
                    f.function.name
                )
            })?;
        }
        for f in self.program.imports.iter() {
            self.generate_import(f)?;
        }
        for e in self.program.enums.iter() {
            self.generate_enum(e);
        }
        for s in self.program.structs.iter() {
            let mut class = self
                .cx
                .exported_classes
                .entry(s.name.clone())
                .or_insert_with(Default::default);
            class.comments = format_doc_comments(&s.comments, None);
            class.fields.extend(s.fields.iter().map(|f| ClassField {
                name: f.name.clone(),
                readonly: f.readonly,
                comments: f.comments.clone(),
            }));
        }

        Ok(())
    }

    fn generate_export(&mut self, export: &shared::Export) -> Result<(), Error> {
        if let Some(ref class) = export.class {
            return self.generate_export_for_class(class, export);
        }

        let descriptor = match self.cx.describe(&export.function.name) {
            None => return Ok(()),
            Some(d) => d,
        };

        let (js, ts, js_doc) = Js2Rust::new(&export.function.name, self.cx)
            .process(descriptor.unwrap_function())?
            .finish("function", &format!("wasm.{}", export.function.name));
        self.cx.export(
            &export.function.name,
            &js,
            Some(format_doc_comments(&export.comments, Some(js_doc))),
        );
        self.cx.globals.push_str("\n");
        self.cx.typescript.push_str("export ");
        self.cx.typescript.push_str(&ts);
        self.cx.typescript.push_str("\n");
        Ok(())
    }

    fn generate_export_for_class(
        &mut self,
        class_name: &str,
        export: &shared::Export,
    ) -> Result<(), Error> {
        let wasm_name = shared::struct_function_export_name(class_name, &export.function.name);

        let descriptor = match self.cx.describe(&wasm_name) {
            None => return Ok(()),
            Some(d) => d,
        };

        let function_name = if export.is_constructor {
            "constructor"
        } else {
            &export.function.name
        };
        let (js, ts, js_doc) = Js2Rust::new(function_name, self.cx)
            .method(export.method, export.consumed)
            .constructor(if export.is_constructor { Some(class_name) } else { None })
            .process(descriptor.unwrap_function())?
            .finish("", &format!("wasm.{}", wasm_name));

        let class = self
            .cx
            .exported_classes
            .entry(class_name.to_string())
            .or_insert(ExportedClass::default());
        class
            .contents
            .push_str(&format_doc_comments(&export.comments, Some(js_doc)));

        if export.is_constructor {
            if class.has_constructor {
                bail!("found duplicate constructor `{}`",
                      export.function.name);
            }
            class.has_constructor = true;
        } else if !export.method {
            class.contents.push_str("static ");
            class.typescript.push_str("static ");
        }

        class.contents.push_str(function_name);
        class.contents.push_str(&js);
        class.contents.push_str("\n");
        class.typescript.push_str(&ts);
        class.typescript.push_str("\n");
        Ok(())
    }

    fn generate_import(&mut self, import: &shared::Import) -> Result<(), Error> {
        match import.kind {
            shared::ImportKind::Function(ref f) => {
                self.generate_import_function(import, f).with_context(|_| {
                    format!(
                        "failed to generate bindings for JS import `{}`",
                        f.function.name
                    )
                })?;
            }
            shared::ImportKind::Static(ref s) => {
                self.generate_import_static(import, s).with_context(|_| {
                    format!("failed to generate bindings for JS import `{}`", s.name)
                })?;
            }
            shared::ImportKind::Type(ref ty) => {
                self.generate_import_type(import, ty).with_context(|_| {
                    format!(
                        "failed to generate bindings for JS import `{}`",
                        ty.name,
                    )
                })?;
            }
            shared::ImportKind::Enum(_) => {}
        }
        Ok(())
    }

    fn generate_import_static(
        &mut self,
        info: &shared::Import,
        import: &shared::ImportStatic,
    ) -> Result<(), Error> {
        // The same static can be imported in multiple locations, so only
        // generate bindings once for it.
        if !self.cx.imported_statics.insert(import.shim.clone()) {
            return Ok(())
        }

        // TODO: should support more types to import here
        let obj = self.import_name(info, &import.name)?;
        self.cx.expose_add_heap_object();
        self.cx.export(
            &import.shim,
            &format!(
                "
                function() {{
                    return addHeapObject({});
                }}
                ",
                obj
            ),
            None,
        );
        Ok(())
    }

    fn generate_import_function(
        &mut self,
        info: &shared::Import,
        import: &shared::ImportFunction,
    ) -> Result<(), Error> {
        if !self.cx.wasm_import_needed(&import.shim) {
            return Ok(());
        }

        // It's possible for the same function to be imported in two locations,
        // but we only want to generate one.
        if !self.cx.imported_functions.insert(import.shim.clone()) {
            return Ok(());
        }

        let descriptor = match self.cx.describe(&import.shim) {
            None => return Ok(()),
            Some(d) => d,
        };

        let target = self.generated_import_target(info, import, &descriptor)?;

        let js = Rust2Js::new(self.cx)
            .catch(import.catch)
            .variadic(import.variadic)
            .process(descriptor.unwrap_function())?
            .finish(&target)?;
        self.cx.export(&import.shim, &js, None);
        Ok(())
    }

    fn generated_import_target(
        &mut self,
        info: &shared::Import,
        import: &shared::ImportFunction,
        descriptor: &Descriptor,
    ) -> Result<String, Error> {
        let method_data = match &import.method {
            Some(data) => data,
            None => {
                let name = self.import_name(info, &import.function.name)?;
                return Ok(if name.contains(".") {
                    self.cx.global(&format!(
                        "
                        const {}_target = {};
                        ",
                        import.shim, name
                    ));
                    format!("{}_target", import.shim)
                } else {
                    name
                })
            }
        };

        let class = self.import_name(info, &method_data.class)?;
        let op = match &method_data.kind {
            shared::MethodKind::Constructor => return Ok(format!("new {}", class)),
            shared::MethodKind::Operation(op) => op,
        };
        let target = if import.structural {
            let location = if op.is_static { &class } else { "this" };

            match &op.kind {
                shared::OperationKind::Regular => {
                    let nargs = descriptor.unwrap_function().arguments.len();
                    let mut s = format!("function(");
                    for i in 0..nargs - 1 {
                        if i > 0 {
                            drop(write!(s, ", "));
                        }
                        drop(write!(s, "x{}", i));
                    }
                    s.push_str(") { \nreturn this.");
                    s.push_str(&import.function.name);
                    s.push_str("(");
                    for i in 0..nargs - 1 {
                        if i > 0 {
                            drop(write!(s, ", "));
                        }
                        drop(write!(s, "x{}", i));
                    }
                    s.push_str(");\n}");
                    s
                }
                shared::OperationKind::Getter(g) => format!(
                    "function() {{
                        return {}.{};
                    }}",
                    location, g
                ),
                shared::OperationKind::Setter(s) => format!(
                    "function(y) {{
                        {}.{} = y;
                    }}",
                    location, s
                ),
                shared::OperationKind::IndexingGetter => format!(
                    "function(y) {{
                        return {}[y];
                    }}",
                    location
                ),
                shared::OperationKind::IndexingSetter => format!(
                    "function(y, z) {{
                        {}[y] = z;
                    }}",
                    location
                ),
                shared::OperationKind::IndexingDeleter => format!(
                    "function(y) {{
                        delete {}[y];
                    }}",
                    location
                ),
            }
        } else {
            let (location, binding) = if op.is_static {
                ("", format!(".bind({})", class))
            } else {
                (".prototype", "".into())
            };

            match &op.kind {
                shared::OperationKind::Regular => {
                    format!("{}{}.{}{}", class, location, import.function.name, binding)
                }
                shared::OperationKind::Getter(g) => {
                    self.cx.expose_get_inherited_descriptor();
                    format!(
                        "GetOwnOrInheritedPropertyDescriptor({}{}, '{}').get{}",
                        class, location, g, binding,
                    )
                }
                shared::OperationKind::Setter(s) => {
                    self.cx.expose_get_inherited_descriptor();
                    format!(
                        "GetOwnOrInheritedPropertyDescriptor({}{}, '{}').set{}",
                        class, location, s, binding,
                    )
                }
                shared::OperationKind::IndexingGetter => panic!("indexing getter should be structural"),
                shared::OperationKind::IndexingSetter => panic!("indexing setter should be structural"),
                shared::OperationKind::IndexingDeleter => panic!("indexing deleter should be structural"),
            }
        };

        let fallback = if import.structural {
            "".to_string()
        } else {
            format!(
                " || function() {{
                    throw new Error(`wasm-bindgen: {} does not exist`);
                }}",
                target
            )
        };

        self.cx.global(&format!(
            "const {}_target = {}{};",
            import.shim, target, fallback
        ));
        Ok(format!(
            "{}_target{}",
            import.shim,
            if op.is_static { "" } else { ".call" }
        ))
    }

    fn generate_import_type(
        &mut self,
        info: &shared::Import,
        import: &shared::ImportType,
    ) -> Result<(), Error> {
        if !self.cx.wasm_import_needed(&import.instanceof_shim) {
            return Ok(());
        }
        let name = self.import_name(info, &import.name)?;
        self.cx.expose_get_object();
        let body = format!("
                function(idx) {{
                    return getObject(idx) instanceof {} ? 1 : 0;
                }}
            ",
            name,
        );
        self.cx.export(&import.instanceof_shim, &body, None);
        Ok(())
    }

    fn generate_enum(&mut self, enum_: &shared::Enum) {
        let mut variants = String::new();

        for variant in enum_.variants.iter() {
            variants.push_str(&format!("{}:{},", variant.name, variant.value));
        }
        self.cx.export(
            &enum_.name,
            &format!("Object.freeze({{ {} }})", variants),
            Some(format_doc_comments(&enum_.comments, None)),
        );
        self.cx
            .typescript
            .push_str(&format!("export enum {} {{", enum_.name));

        variants.clear();
        for variant in enum_.variants.iter() {
            variants.push_str(&format!("{},", variant.name));
        }
        self.cx.typescript.push_str(&variants);
        self.cx.typescript.push_str("}\n");
    }

    fn import_name(&mut self, import: &shared::Import, item: &str) -> Result<String, Error> {
        // First up, imports don't work at all in `--no-modules` mode as we're
        // not sure how to import them.
        if self.cx.config.no_modules {
            if let Some(module) = &import.module {
                bail!(
                    "import from `{}` module not allowed with `--no-modules`; \
                     use `--nodejs` or `--browser` instead",
                    module
                );
            }
        }

        // Figure out what identifier we're importing from the module. If we've
        // got a namespace we use that, otherwise it's the name specified above.
        let name_to_import = import.js_namespace
            .as_ref()
            .map(|s| &**s)
            .unwrap_or(item);

        // Here's where it's a bit tricky. We need to make sure that importing
        // the same identifier from two different modules works, and they're
        // named uniquely below. Additionally if we've already imported the same
        // identifier from the module in question then we'd like to reuse the
        // one that was previously imported.
        //
        // Our `imported_names` map keeps track of all imported identifiers from
        // modules, mapping the imported names onto names actually available for
        // use in our own module. If our identifier isn't present then we
        // generate a new identifier and are sure to generate the appropriate JS
        // import for our new identifier.
        let use_node_require = self.cx.use_node_require();
        let imported_identifiers = &mut self.cx.imported_identifiers;
        let imports = &mut self.cx.imports;
        let identifier = self.cx.imported_names.entry(import.module.clone())
            .or_insert_with(Default::default)
            .entry(name_to_import.to_string())
            .or_insert_with(|| {
                let name = generate_identifier(name_to_import, imported_identifiers);
                if let Some(module) = &import.module {
                    if use_node_require {
                        imports.push_str(&format!(
                            "const {} = require(String.raw`{}`).{};\n",
                            name, module, name_to_import
                        ));
                    } else if name_to_import == name {
                        imports.push_str(&format!(
                            "import {{ {} }} from '{}';\n",
                            name, module
                        ));
                    } else {
                        imports.push_str(&format!(
                            "import {{ {} as {} }} from '{}';\n",
                            name_to_import, name, module
                        ));
                    }
                }
                name
            });

        // If there's a namespace we didn't actually import `item` but rather
        // the namespace, so access through that.
        if import.js_namespace.is_some() {
            Ok(format!("{}.{}", identifier, item))
        } else {
            Ok(identifier.to_string())
        }
    }
}

fn generate_identifier(name: &str, used_names: &mut HashMap<String, usize>) -> String {
    let cnt = used_names.entry(name.to_string()).or_insert(0);
    *cnt += 1;
    if *cnt == 1 {
        name.to_string()
    } else {
        format!("{}{}", name, cnt)
    }
}

fn format_doc_comments(comments: &Vec<String>, js_doc_comments: Option<String>) -> String {
    let body: String = comments
        .iter()
        .map(|c| format!("*{}\n", c.trim_matches('"')))
        .collect();
    let doc = if let Some(docs) = js_doc_comments {
        docs.lines().map(|l| format!("* {} \n", l)).collect()
    } else {
        String::new()
    };
    format!("/**\n{}{}*/\n", body, doc)
}
