; Only root object entries are independent semantic definitions. Nested values
; stay inside their owning entry, matching the document indexing boundary.
(document (object (pair
    key: (string) @name.definition.json.object
    value: [(object) (array)]) @definition.json.object))

(document (object (pair
    key: (string) @name.definition.json.property
    value: [(string) (number) (true) (false) (null)]) @definition.json.property))
