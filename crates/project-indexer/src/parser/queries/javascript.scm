; Constructor methods in classes
(method_definition
  name: (property_identifier) @name.definition.callable.constructor
  (#eq? @name.definition.callable.constructor "constructor")) @definition.callable.constructor

; Regular method definitions
(method_definition
 name: (property_identifier) @name.definition.callable.method) @definition.callable.method

; Class field definitions
(field_definition
 property: (property_identifier) @name.definition.data.field) @definition.data.field

(class_declaration
 name: (identifier) @name.definition.container.type.class) @definition.container.type.class

; Class inheritance - extends clause
(class_declaration
  (class_heritage
    (identifier) @name.reference.container.type)) @reference.container.type

; Function declarations (including async)
(function_declaration
 name: (identifier) @name.definition.callable.function) @definition.callable.function

(arrow_function
 parameter: (identifier) @name.definition.data.parameter) @definition.data.parameter

(formal_parameters
  (identifier) @name.definition.data.parameter) @definition.data.parameter

(variable_declarator
 name: (identifier) @name.definition.callable.function
 value: [(function_expression) (arrow_function)]) @definition.callable.function

(call_expression
 function: [
   (identifier) @name.reference.callable
   (member_expression
     property: (property_identifier) @name.reference.callable)
 ]) @reference.callable

(new_expression
 constructor: (identifier) @name.reference.container.type) @reference.container.type
