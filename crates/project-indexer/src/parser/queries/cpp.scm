; Struct definitions
(struct_specifier name: (type_identifier) @name.definition.container.type.struct body:(_)) @definition.container.type.struct

; Field declarations in structs/unions/classes
(field_declaration declarator: (field_identifier) @name.definition.data.field) @definition.data.field

; Union definitions
(declaration type: (union_specifier name: (type_identifier) @name.definition.container.type.union)) @definition.container.type.union

; Function definitions
(function_declarator declarator: (identifier) @name.definition.callable.function) @definition.callable.function

; Function definitions with field identifier
(function_declarator declarator: (field_identifier) @name.definition.callable.function) @definition.callable.function

; Method definitions with namespace qualifier
(function_declarator declarator: (qualified_identifier scope: (namespace_identifier) @local.scope name: (identifier) @name.definition.callable.method)) @definition.callable.method

; Type aliases (typedef)
(type_definition declarator: (type_identifier) @name.definition.container.type.alias) @definition.container.type.alias

; Enum definitions
(enum_specifier name: (type_identifier) @name.definition.container.type.enum) @definition.container.type.enum

; Class definitions
(class_specifier name: (type_identifier) @name.definition.container.type.class) @definition.container.type.class

; Namespace definitions
(namespace_definition
  name: (namespace_identifier) @name.definition.container.namespace) @definition.container.namespace

; Enum constant definitions (enumerators)
(enumerator name: (identifier) @name.definition.data.constant) @definition.data.constant

; Function parameters
(parameter_declaration
  declarator: (identifier) @name.definition.data.parameter) @definition.data.parameter

; Reference parameters
(parameter_declaration
  declarator: (reference_declarator
    (identifier) @name.definition.data.parameter)) @definition.data.parameter

; Pointer parameters
(parameter_declaration
  declarator: (pointer_declarator
    declarator: (identifier) @name.definition.data.parameter)) @definition.data.parameter

; Function call references
(call_expression
  function: (identifier) @name.reference.callable) @reference.callable

; Method call references
(call_expression
  function: (field_expression
    field: (field_identifier) @name.reference.callable)) @reference.callable

; Scoped function call references
(call_expression
  function: (qualified_identifier
    name: (identifier) @name.reference.callable)) @reference.callable

; Constructor call with new
(new_expression
  type: (type_identifier) @name.reference.container.type) @reference.container.type

; Template class instantiation
(template_type
  name: (type_identifier) @name.reference.container.type) @reference.container.type
