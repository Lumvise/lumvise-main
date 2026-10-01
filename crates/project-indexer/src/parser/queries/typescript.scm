; Constructor methods in classes
(method_definition
  name: (property_identifier) @name.definition.callable.constructor
  (#eq? @name.definition.callable.constructor "constructor")) @definition.callable.constructor

; Regular method definitions
(method_definition
 name: (property_identifier) @name.definition.callable.method) @definition.callable.method

; Class properties/fields
(public_field_definition
 name: (property_identifier) @name.definition.data.field) @definition.data.field

(class_declaration
 name: (type_identifier) @name.definition.container.type.class) @definition.container.type.class

; Abstract class declarations
(abstract_class_declaration
 name: (type_identifier) @name.definition.container.type.class) @definition.container.type.class

; Class inheritance - extends clause
(class_declaration
  (class_heritage
    (extends_clause
      value: (identifier) @name.reference.container.type))) @reference.container.type

; Class inheritance - implements clause
(class_declaration
  (class_heritage
    (implements_clause
      (type_identifier) @name.reference.container.type.interface))) @reference.container.type.interface

(interface_declaration
 name: (type_identifier) @name.definition.container.type.interface) @definition.container.type.interface

(type_alias_declaration
 name: (type_identifier) @name.definition.container.type.alias) @definition.container.type.alias

; Enum declarations
(enum_declaration
  name: (identifier) @name.definition.container.type.enum) @definition.container.type.enum

; Enum member declarations
(enum_assignment
  name: (property_identifier) @name.definition.data.constant) @definition.data.constant

(function_declaration
 name: (identifier) @name.definition.callable.function) @definition.callable.function

(formal_parameters
  (required_parameter
    pattern: (identifier) @name.definition.data.parameter)) @definition.data.parameter

(variable_declarator
 name: (identifier) @name.definition.callable.function
 value: [(function_expression) (arrow_function)]) @definition.callable.function

(lexical_declaration
 (variable_declarator
  name: (identifier) @name.definition.callable.function
  value: (arrow_function))) @definition.callable.function

(call_expression
 function: [
   (identifier) @name.reference.callable
   (member_expression
     property: (property_identifier) @name.reference.callable)
 ]) @reference.callable

(new_expression
 constructor: (identifier) @name.reference.container.type) @reference.container.type

; Import declarations (treat as module references)
(import_statement
 (import_clause
  (named_imports
   (import_specifier
    name: (identifier) @name.reference.container.module)))) @reference.container.module

(import_statement
 (import_clause
  (identifier) @name.reference.container.module)) @reference.container.module

; Export declarations - definitions are already captured above, no need for export-specific tags
