; Based on https://github.com/tree-sitter/tree-sitter-c-sharp/blob/master/queries/tags.scm
; MIT License.

; Attributes (C# decorators - capture entire attribute_list for association with definitions)
(attribute_list) @decorator

(class_declaration name: (identifier) @name.definition.container.type.class) @definition.container.type.class

; Class inheritance - simple types
(class_declaration
  (base_list
    (identifier) @name.reference.container.type)) @reference.container.type

; Class inheritance - generic types like IRepository<T>
(class_declaration
  (base_list
    (generic_name
      (identifier) @name.reference.container.type))) @reference.container.type

(struct_declaration name: (identifier) @name.definition.container.type.struct) @definition.container.type.struct

; Struct inheritance - simple types
(struct_declaration
  (base_list
    (identifier) @name.reference.container.type)) @reference.container.type

; Struct inheritance - generic types
(struct_declaration
  (base_list
    (generic_name
      (identifier) @name.reference.container.type))) @reference.container.type

(enum_declaration name: (identifier) @name.definition.container.type.enum) @definition.container.type.enum

(interface_declaration name: (identifier) @name.definition.container.type.interface) @definition.container.type.interface

; Interface inheritance - simple types
(interface_declaration
  (base_list
    (identifier) @name.reference.container.type.interface)) @reference.container.type.interface

; Interface inheritance - generic types
(interface_declaration
  (base_list
    (generic_name
      (identifier) @name.reference.container.type.interface))) @reference.container.type.interface

(method_declaration name: (identifier) @name.definition.callable.method) @definition.callable.method

(constructor_declaration name: (identifier) @name.definition.callable.constructor) @definition.callable.constructor

(property_declaration name: (identifier) @name.definition.data.property) @definition.data.property

(enum_member_declaration name: (identifier) @name.definition.data.constant) @definition.data.constant

(field_declaration
  (variable_declaration
    (variable_declarator name: (identifier) @name.definition.data.field))) @definition.data.field

(object_creation_expression type: (identifier) @name.reference.container.type) @reference.container.type

(type_parameter_constraints_clause (identifier) @name.reference.container.type) @reference.container.type

(type_parameter_constraint (type type: (identifier) @name.reference.container.type)) @reference.container.type

(variable_declaration type: (identifier) @name.reference.container.type) @reference.container.type

(invocation_expression function: (member_access_expression name: (identifier) @name.reference.callable)) @reference.callable

; Local calls have no member-access wrapper.
(invocation_expression function: (identifier) @name.reference.callable) @reference.callable

(namespace_declaration name: (identifier) @name.definition.container.namespace) @definition.container.namespace

; Record declarations (C# 9+)
(record_declaration name: (identifier) @name.definition.container.type.record) @definition.container.type.record
