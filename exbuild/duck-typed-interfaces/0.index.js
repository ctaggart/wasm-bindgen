(window["webpackJsonp"] = window["webpackJsonp"] || []).push([[0],{

/***/ "./duck-typed-interfaces.js":
/*!**********************************!*\
  !*** ./duck-typed-interfaces.js ***!
  \**********************************/
/*! no exports provided */
/***/ (function(module, __webpack_exports__, __webpack_require__) {

"use strict";
eval("__webpack_require__.r(__webpack_exports__);\n/* harmony import */ var _rust_duck_typed_interfaces__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./rust_duck_typed_interfaces */ \"./rust_duck_typed_interfaces.js\");\n\n\n// All of these objects implement the `Quacks` interface!\n\nconst alex = {\n  quack: () => \"you're not wrong...\"\n};\n\nconst ashley = {\n  quack: () => \"<corgi.gif>\"\n};\n\nconst nick = {\n  quack: () => \"rappers I monkey-flip em with the funky rhythm I be kickin\"\n};\n\n// Get all our ducks in a row and call into wasm!\n\nObject(_rust_duck_typed_interfaces__WEBPACK_IMPORTED_MODULE_0__[\"make_em_quack_to_this\"])(alex);\nObject(_rust_duck_typed_interfaces__WEBPACK_IMPORTED_MODULE_0__[\"make_em_quack_to_this\"])(ashley);\nObject(_rust_duck_typed_interfaces__WEBPACK_IMPORTED_MODULE_0__[\"make_em_quack_to_this\"])(nick);\n\n\n//# sourceURL=webpack:///./duck-typed-interfaces.js?");

/***/ }),

/***/ "./rust_duck_typed_interfaces.js":
/*!***************************************!*\
  !*** ./rust_duck_typed_interfaces.js ***!
  \***************************************/
/*! exports provided: __wbg_quack_35d9642b5cccc1d1, make_em_quack_to_this, __wbindgen_throw */
/***/ (function(module, __webpack_exports__, __webpack_require__) {

"use strict";
eval("__webpack_require__.r(__webpack_exports__);\n/* harmony export (binding) */ __webpack_require__.d(__webpack_exports__, \"__wbg_quack_35d9642b5cccc1d1\", function() { return __wbg_quack_35d9642b5cccc1d1; });\n/* harmony export (binding) */ __webpack_require__.d(__webpack_exports__, \"make_em_quack_to_this\", function() { return make_em_quack_to_this; });\n/* harmony export (binding) */ __webpack_require__.d(__webpack_exports__, \"__wbindgen_throw\", function() { return __wbindgen_throw; });\n/* harmony import */ var _rust_duck_typed_interfaces_bg__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./rust_duck_typed_interfaces_bg */ \"./rust_duck_typed_interfaces_bg.wasm\");\n/* tslint:disable */\n\n\nconst __wbg_quack_35d9642b5cccc1d1_target = function() {\n    return this.quack();\n};\n\nconst stack = [];\n\nconst slab = [{ obj: undefined }, { obj: null }, { obj: true }, { obj: false }];\n\nfunction getObject(idx) {\n    if ((idx & 1) === 1) {\n        return stack[idx >> 1];\n    } else {\n        const val = slab[idx >> 1];\n\n        return val.obj;\n\n    }\n}\n\nconst TextEncoder = typeof self === 'object' && self.TextEncoder\n    ? self.TextEncoder\n    : __webpack_require__(/*! util */ \"../../node_modules/util/util.js\").TextEncoder;\n\nlet cachedEncoder = new TextEncoder('utf-8');\n\nlet cachegetUint8Memory = null;\nfunction getUint8Memory() {\n    if (cachegetUint8Memory === null || cachegetUint8Memory.buffer !== _rust_duck_typed_interfaces_bg__WEBPACK_IMPORTED_MODULE_0__[\"memory\"].buffer) {\n        cachegetUint8Memory = new Uint8Array(_rust_duck_typed_interfaces_bg__WEBPACK_IMPORTED_MODULE_0__[\"memory\"].buffer);\n    }\n    return cachegetUint8Memory;\n}\n\nfunction passStringToWasm(arg) {\n\n    const buf = cachedEncoder.encode(arg);\n    const ptr = _rust_duck_typed_interfaces_bg__WEBPACK_IMPORTED_MODULE_0__[\"__wbindgen_malloc\"](buf.length);\n    getUint8Memory().set(buf, ptr);\n    return [ptr, buf.length];\n}\n\nlet cachegetUint32Memory = null;\nfunction getUint32Memory() {\n    if (cachegetUint32Memory === null || cachegetUint32Memory.buffer !== _rust_duck_typed_interfaces_bg__WEBPACK_IMPORTED_MODULE_0__[\"memory\"].buffer) {\n        cachegetUint32Memory = new Uint32Array(_rust_duck_typed_interfaces_bg__WEBPACK_IMPORTED_MODULE_0__[\"memory\"].buffer);\n    }\n    return cachegetUint32Memory;\n}\n\nfunction __wbg_quack_35d9642b5cccc1d1(ret, arg0) {\n\n    const [retptr, retlen] = passStringToWasm(__wbg_quack_35d9642b5cccc1d1_target.call(getObject(arg0)));\n    const mem = getUint32Memory();\n    mem[ret / 4] = retptr;\n    mem[ret / 4 + 1] = retlen;\n\n}\n\nfunction addBorrowedObject(obj) {\n    stack.push(obj);\n    return ((stack.length - 1) << 1) | 1;\n}\n/**\n* Next, we can export a function that takes any object that quacks:\n* @param {any} arg0\n* @returns {void}\n*/\nfunction make_em_quack_to_this(arg0) {\n    try {\n        return _rust_duck_typed_interfaces_bg__WEBPACK_IMPORTED_MODULE_0__[\"make_em_quack_to_this\"](addBorrowedObject(arg0));\n\n    } finally {\n        stack.pop();\n\n    }\n\n}\n\nconst TextDecoder = typeof self === 'object' && self.TextDecoder\n    ? self.TextDecoder\n    : __webpack_require__(/*! util */ \"../../node_modules/util/util.js\").TextDecoder;\n\nlet cachedDecoder = new TextDecoder('utf-8');\n\nfunction getStringFromWasm(ptr, len) {\n    return cachedDecoder.decode(getUint8Memory().subarray(ptr, ptr + len));\n}\n\nfunction __wbindgen_throw(ptr, len) {\n    throw new Error(getStringFromWasm(ptr, len));\n}\n\n\n\n//# sourceURL=webpack:///./rust_duck_typed_interfaces.js?");

/***/ }),

/***/ "./rust_duck_typed_interfaces_bg.wasm":
/*!********************************************!*\
  !*** ./rust_duck_typed_interfaces_bg.wasm ***!
  \********************************************/
/*! exports provided: memory, __indirect_function_table, __heap_base, __data_end, __rustc_debug_gdb_scripts_section__, make_em_quack_to_this, __wbindgen_malloc */
/***/ (function(module, exports, __webpack_require__) {

eval("\"use strict\";\n// Instantiate WebAssembly module\nvar wasmExports = __webpack_require__.w[module.i];\n__webpack_require__.r(exports);\n// export exports from WebAssembly module\nfor(var name in wasmExports) if(name != \"__webpack_init__\") exports[name] = wasmExports[name];\n// exec imports from WebAssembly module (for esm order)\n/* harmony import */ var m0 = __webpack_require__(/*! ./rust_duck_typed_interfaces */ \"./rust_duck_typed_interfaces.js\");\n\n\n// exec wasm module\nwasmExports[\"__webpack_init__\"]()\n\n//# sourceURL=webpack:///./rust_duck_typed_interfaces_bg.wasm?");

/***/ })

}]);