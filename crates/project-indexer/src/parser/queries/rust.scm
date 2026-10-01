; ADT definitions

(struct_item
    name: (type_identifier) @name.definition.container.type.struct) @definition.container.type.struct

; Struct field declarations
(field_declaration
    name: (field_identifier) @name.definition.data.field) @definition.data.field

(enum_item
    name: (type_identifier) @name.definition.container.type.enum) @definition.container.type.enum

; Enum variant definitions
(enum_variant
    name: (identifier) @name.definition.data.constant) @definition.data.constant

(union_item
    name: (type_identifier) @name.definition.container.type.union) @definition.container.type.union

; type aliases

(type_item
    name: (type_identifier) @name.definition.container.type.alias) @definition.container.type.alias

; Keep non-callable declarations in the same syntax pass as functions.
(const_item
    name: (identifier) @name.definition.data.constant) @definition.data.constant

(static_item
    name: (identifier) @name.definition.data.static) @definition.data.static

; method definitions

(impl_item
    body: (declaration_list
        (function_item
            name: (identifier) @name.definition.callable.method) @definition.callable.method))

(trait_item
    body: (declaration_list
        (function_item
            name: (identifier) @name.definition.callable.method) @definition.callable.method))

; function definitions

(function_item
    name: (identifier) @name.definition.callable.function) @definition.callable.function

(function_signature_item
    name: (identifier) @name.definition.callable.function) @definition.callable.function

; trait definitions
(trait_item
    name: (type_identifier) @name.definition.container.type.trait) @definition.container.type.trait

; module definitions
(mod_item
    name: (identifier) @name.definition.container.module) @definition.container.module

; macro definitions

(macro_definition
    name: (identifier) @name.definition.callable.macro) @definition.callable.macro

; references

(call_expression
    function: (identifier) @name.reference.callable) @reference.callable

(call_expression
    function: (field_expression
        field: (field_identifier) @name.reference.callable)) @reference.callable

(macro_invocation
    macro: (identifier) @name.reference.callable.macro) @reference.callable.macro

; implementations

(impl_item
    trait: (type_identifier) @name.reference.container.type) @reference.container.type

(impl_item
    type: (type_identifier) @name.reference.container.type
    !trait) @reference.container.type

; Keep qualified calls available for project reference resolution.
(call_expression
    function: (scoped_identifier
        name: (identifier) @name.reference.callable)) @reference.callable
