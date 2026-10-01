; Heading units are the document outline. Heading text is the inline child,
; so empty headings collapse into their parent section and are skipped.
(atx_heading
    (inline) @name.definition.markdown.heading) @definition.markdown.heading

(setext_heading
    (paragraph) @name.definition.markdown.heading) @definition.markdown.heading
