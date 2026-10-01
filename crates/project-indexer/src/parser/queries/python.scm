; Module-level constant (top-level assignment)
(module (expression_statement (assignment left: (identifier) @name.definition.data.constant) @definition.data.constant))

; Class field (class-level variable assignments)
(class_definition
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @name.definition.data.field)) @definition.data.field))

; Instance field (self.x = y pattern)
(assignment
  left: (attribute
    object: (identifier) @_self
    attribute: (identifier) @name.definition.data.field) @definition.data.field
  (#eq? @_self "self"))

; Decorated class definitions
(decorated_definition
  (decorator)* @decorator
  definition: (class_definition
    name: (identifier) @name.definition.container.type.class)) @definition.container.type.class

; Non-decorated class definitions
(class_definition
 name: (identifier) @name.definition.container.type.class) @definition.container.type.class

; Base class references in class definitions (inheritance)
(class_definition
  superclasses: (argument_list
    (identifier) @name.reference.container.type)) @reference.container.type

; Decorated constructor definitions (__init__)
(decorated_definition
  (decorator)* @decorator
  definition: (function_definition
    name: (identifier) @name.definition.callable.constructor
    (#eq? @name.definition.callable.constructor "__init__"))) @definition.callable.constructor

; Non-decorated constructor definitions (__init__)
(function_definition
  name: (identifier) @name.definition.callable.constructor
  (#eq? @name.definition.callable.constructor "__init__")) @definition.callable.constructor

; Decorated function definitions (including async)
(decorated_definition
  (decorator)* @decorator
  definition: (function_definition
    name: (identifier) @name.definition.callable.function)) @definition.callable.function

; Non-decorated function definitions (including async)
(function_definition
 name: (identifier) @name.definition.callable.function) @definition.callable.function

; Method definitions inside class body (excluding __init__ which is constructor)
; These patterns come AFTER function patterns so method tags override function tags
; for the same node (HashMap insert behavior)
(class_definition
  body: (block
    (function_definition
      name: (identifier) @name.definition.callable.method
      (#not-eq? @name.definition.callable.method "__init__")) @definition.callable.method))

; Decorated method definitions inside class body
(class_definition
  body: (block
    (decorated_definition
      definition: (function_definition
        name: (identifier) @name.definition.callable.method
        (#not-eq? @name.definition.callable.method "__init__"))) @definition.callable.method))

; Function/method parameters (excluding self/cls)
(parameters
  (identifier) @name.definition.data.parameter
  (#not-eq? @name.definition.data.parameter "self")
  (#not-eq? @name.definition.data.parameter "cls")) @definition.data.parameter

; Typed parameters (e.g., param: str)
(parameters
  (typed_parameter
    (identifier) @name.definition.data.parameter
    (#not-eq? @name.definition.data.parameter "self")
    (#not-eq? @name.definition.data.parameter "cls"))) @definition.data.parameter

; Default parameters (e.g., param=value)
(parameters
  (default_parameter
    name: (identifier) @name.definition.data.parameter
    (#not-eq? @name.definition.data.parameter "self")
    (#not-eq? @name.definition.data.parameter "cls"))) @definition.data.parameter

; Typed default parameters (e.g., param: str = "default")
(parameters
  (typed_default_parameter
    name: (identifier) @name.definition.data.parameter
    (#not-eq? @name.definition.data.parameter "self")
    (#not-eq? @name.definition.data.parameter "cls"))) @definition.data.parameter

(call
 function: [
 (identifier) @name.reference.callable
 (attribute
 attribute: (identifier) @name.reference.callable)
 ]) @reference.callable
