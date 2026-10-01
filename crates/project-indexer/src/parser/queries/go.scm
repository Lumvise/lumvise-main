; Package declarations
(package_clause
  (package_identifier) @name.definition.container.module) @definition.container.module

(
  (comment)* @doc
  .
  (function_declaration
    name: (identifier) @name.definition.callable.function) @definition.callable.function
  (#strip! @doc "^//\\s*")
  (#set-adjacent! @doc @definition.callable.function)
)

(
  (comment)* @doc
  .
  (method_declaration
    receiver: (parameter_list
      (parameter_declaration
        type: [
          (type_identifier) @receiver_type
          (pointer_type (type_identifier) @receiver_type)
        ]))
    name: (field_identifier) @name.definition.callable.method) @definition.callable.method
  (#strip! @doc "^//\\s*")
  (#set-adjacent! @doc @definition.callable.method)
)

(call_expression
  function: [
    (identifier) @name.reference.callable
    (parenthesized_expression (identifier) @name.reference.callable)
    (selector_expression field: (field_identifier) @name.reference.callable)
    (parenthesized_expression (selector_expression field: (field_identifier) @name.reference.callable))
  ]) @reference.callable

; Struct definitions with doc comments
(
  (comment)* @doc
  .
  (type_declaration
    (type_spec
      name: (type_identifier) @name.definition.container.type.struct
      type: (struct_type))) @definition.container.type.struct
  (#strip! @doc "^//\\s*")
  (#set-adjacent! @doc @definition.container.type.struct)
)

; Struct definitions without doc comments
(type_declaration
  (type_spec
    name: (type_identifier) @name.definition.container.type.struct
    type: (struct_type))) @definition.container.type.struct

; Interface definitions with doc comments
(
  (comment)* @doc
  .
  (type_declaration
    (type_spec
      name: (type_identifier) @name.definition.container.type.interface
      type: (interface_type))) @definition.container.type.interface
  (#strip! @doc "^//\\s*")
  (#set-adjacent! @doc @definition.container.type.interface)
)

; Interface definitions without doc comments
(type_declaration
  (type_spec
    name: (type_identifier) @name.definition.container.type.interface
    type: (interface_type))) @definition.container.type.interface

; Struct field declarations
(field_declaration
  name: (field_identifier) @name.definition.data.field) @definition.data.field

; Embedded types in structs (Go's composition/inheritance)
(field_declaration
  type: (type_identifier) @name.reference.container.type) @reference.container.type

; Other type definitions (aliases, etc.) - excluding struct/interface which are captured above
(type_declaration
  (type_spec
    name: (type_identifier) @name.definition.container.type.alias
    type: [
      (type_identifier)
      (array_type)
      (slice_type)
      (map_type)
      (channel_type)
      (function_type)
      (pointer_type)
      (qualified_type)
    ])) @definition.container.type.alias

; Type references
(type_identifier) @name.reference.container.type @reference.container.type
