/* Minimal xmlversion.h for freestanding AtomOS build of libxml2. */

#ifndef __XML_VERSION_H__
#define __XML_VERSION_H__

/* puxa XMLPUBFUN, XMLCALL, etc. */
#include <libxml/xmlexports.h>

/* Versão fixa (pode ser ajustada conforme o upstream clonado) */
#define LIBXML_DOTTED_VERSION "2.12.0"
#define LIBXML_VERSION 2012000
#define LIBXML_VERSION_STRING "2012000"
#define LIBXML_VERSION_EXTRA ""

/* Teste de versão em runtime (no-op simples aqui) */
#define LIBXML_TEST_VERSION xmlCheckVersion(LIBXML_VERSION)

/* Habilitações de módulo (mantidas bem conservadoras) */
#define LIBXML_TREE_ENABLED
#define LIBXML_OUTPUT_ENABLED
#define LIBXML_PUSH_ENABLED
#define LIBXML_READER_ENABLED
#define LIBXML_VALID_ENABLED
#define LIBXML_XINCLUDE_ENABLED
#define LIBXML_XPATH_ENABLED
#define LIBXML_XPTR_ENABLED

/* Desliga partes que não queremos/precisamos neste port minimal */
/* #define LIBXML_HTML_ENABLED */
/* #define LIBXML_CATALOG_ENABLED */

/* Compat flags usados em headers (deixar zlib/icov desligados por ausência de libs) */
/* não definimos LIBXML_ICONV_ENABLED para evitar #ifdef que requerem <iconv.h> */
/* não definimos LIBXML_ZLIB_ENABLED; zlib é tratado via NO_ZLIB nos CFLAGS */

#endif /* __XML_VERSION_H__ */


