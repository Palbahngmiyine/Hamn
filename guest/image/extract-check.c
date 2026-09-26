/* Linux image-builder entrypoint for the same decoder shipped in Hamn. */
#include <stdio.h>
#include <stdlib.h>
#include "image/qcow2.h"
int main(int argc, char **argv)
{
    if (argc != 3) return 2;
    char *error = NULL;
    int rc = qcow2_extract(argv[1], argv[2], &error);
    if (rc != 0) fprintf(stderr, "%s\n", error ? error : "extraction failed");
    free(error);
    return rc == 0 ? 0 : 1;
}
