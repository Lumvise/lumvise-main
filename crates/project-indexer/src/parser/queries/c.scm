; Struct definitions
(struct_specifier name: (type_identifier) @name.definition.container.type.struct body:(_)) @definition.container.type.struct

; Field declarations in structs/unions
(field_declaration declarator: (field_identifier) @name.definition.data.field) @definition.data.field

; Union definitions
(declaration type: (union_specifier name: (type_identifier) @name.definition.container.type.union)) @definition.container.type.union

; Function definitions
(function_declarator declarator: (identifier) @name.definition.callable.function) @definition.callable.function

; Type aliases (typedef)
(type_definition declarator: (type_identifier) @name.definition.container.type.alias) @definition.container.type.alias

; Enum definitions
(enum_specifier name: (type_identifier) @name.definition.container.type.enum) @definition.container.type.enum

; Enum constant definitions (enumerators)
(enumerator name: (identifier) @name.definition.data.constant) @definition.data.constant

; Function parameters
(parameter_declaration
  declarator: (identifier) @name.definition.data.parameter) @definition.data.parameter

; Pointer function parameters
(parameter_declaration
  declarator: (pointer_declarator
    declarator: (identifier) @name.definition.data.parameter)) @definition.data.parameter

; Function call references
(call_expression
  function: (identifier) @name.reference.callable) @reference.callable

; Member function call references
(call_expression
  function: (field_expression
    field: (field_identifier) @name.reference.callable)) @reference.callable

; Variable references
(identifier) @name.reference.data

; Global variable definitions
(declaration
  declarator: (init_declarator
    declarator: (identifier) @name.definition.data.variable)) @definition.data.variable
