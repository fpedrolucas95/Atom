/* Minimal config.h for libxml2 built freestanding inside AtomOS.
 * Ajuste conforme necessário se surgirem erros de compilação. */

#ifndef __ATOMOS_LIBXML2_CONFIG_H__
#define __ATOMOS_LIBXML2_CONFIG_H__

#define LIBXML_CATALOG_ENABLED 0
#define LIBXML_HTML_ENABLED 0
#define LIBXML_HTTP_ENABLED 0
#define LIBXML_OUTPUT_ENABLED 1
#define LIBXML_PUSH_ENABLED 1
#define LIBXML_READER_ENABLED 1
#define LIBXML_REGEXP_ENABLED 1
#define LIBXML_SCHEMATRON_ENABLED 0
#define LIBXML_TEST_ENABLED 0
#define LIBXML_TREE_ENABLED 1
#define LIBXML_VALID_ENABLED 1
#define LIBXML_WRITER_ENABLED 1
#define LIBXML_XINCLUDE_ENABLED 1
#define LIBXML_XPATH_ENABLED 1
#define LIBXML_XPTR_ENABLED 1

#define HAVE_STDINT_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STRING_H 1

#endif /* __ATOMOS_LIBXML2_CONFIG_H__ */

