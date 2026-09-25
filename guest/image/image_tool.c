/*
 * hamn-image-tool: guest image evidence and size gates for the Linux image
 * builder and release publication. Built from guest/Makefile with only a C
 * compiler and libc (no Python, Rust or zlib); see docs/RELEASE-SETUP.md.
 *
 *   evidence check DESTINATION [RESERVED...]
 *   evidence publish SOURCE DESTINATION SIZE_REPORT
 *   verify-raw RAW_IMAGE
 *   verify-size --baseline F --candidate F --packages-before F
 *               --packages-after F --report F --budget F
 *               --base-sha256 HEX --source-revision HEX [--review-only]
 *   verify-release-size IMAGE SIZE_REPORT REVIEWED_BUDGET [SOURCE_REVISION]
 *   variations --source-root DIR --baseline F --size-report F
 *              --output-directory DIR [--seed N] [--case-count N]
 *
 * Exit status: 0 success, 1 rejected evidence or I/O failure with one
 * prefixed line on stderr, 2 usage error.
 */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "image/evidence.h"
#include "image/raw_check.h"
#include "image/size_gate.h"
#include "image/variations.h"

#define EXIT_REJECTED 1
#define EXIT_USAGE 2

static int usage(const char *problem)
{
    if (problem)
        fprintf(stderr, "hamn-image-tool: %s\n", problem);
    fprintf(stderr,
        "usage: hamn-image-tool evidence check DESTINATION [RESERVED...]\n"
        "       hamn-image-tool evidence publish SOURCE DESTINATION SIZE_REPORT\n"
        "       hamn-image-tool verify-raw RAW_IMAGE\n"
        "       hamn-image-tool verify-size --baseline F --candidate F\n"
        "           --packages-before F --packages-after F --report F --budget F\n"
        "           --base-sha256 HEX --source-revision HEX [--review-only]\n"
        "       hamn-image-tool verify-release-size IMAGE SIZE_REPORT"
        " REVIEWED_BUDGET [SOURCE_REVISION]\n"
        "       hamn-image-tool variations --source-root DIR --baseline F\n"
        "           --size-report F --output-directory DIR [--seed N]"
        " [--case-count N]\n");
    return EXIT_USAGE;
}

static int finish(int rc, const char *prefix, const struct image_error *error)
{
    if (rc == 0)
        return 0;
    fprintf(stderr, "%s%s\n", prefix, error->message);
    return EXIT_REJECTED;
}

struct option_spec {
    const char *name;   /* without the leading "--" */
    int flag;           /* takes no value */
    const char *value;  /* parsed value, or "" for a present flag */
};

/*
 * Parses "--name VALUE", "--name=VALUE" and flags. Unknown, repeated and
 * valueless options are usage errors.
 */
static int parse_options(int argc, char **argv, struct option_spec *specs,
                         size_t count)
{
    for (int index = 0; index < argc; index++) {
        const char *argument = argv[index];
        if (strncmp(argument, "--", 2) != 0) {
            fprintf(stderr, "hamn-image-tool: unexpected argument: %s\n",
                    argument);
            return -1;
        }
        const char *name = argument + 2;
        const char *equals = strchr(name, '=');
        size_t name_length = equals ? (size_t)(equals - name) : strlen(name);
        struct option_spec *spec = NULL;
        for (size_t option = 0; option < count; option++) {
            if (strlen(specs[option].name) == name_length &&
                strncmp(specs[option].name, name, name_length) == 0)
                spec = &specs[option];
        }
        if (!spec) {
            fprintf(stderr, "hamn-image-tool: unknown option: %s\n", argument);
            return -1;
        }
        if (spec->value) {
            fprintf(stderr, "hamn-image-tool: repeated option: --%s\n",
                    spec->name);
            return -1;
        }
        if (spec->flag) {
            if (equals) {
                fprintf(stderr, "hamn-image-tool: --%s takes no value\n",
                        spec->name);
                return -1;
            }
            spec->value = "";
        } else if (equals) {
            spec->value = equals + 1;
        } else if (index + 1 < argc) {
            spec->value = argv[++index];
        } else {
            fprintf(stderr, "hamn-image-tool: --%s requires a value\n",
                    spec->name);
            return -1;
        }
    }
    return 0;
}

static int require_options(const struct option_spec *specs, size_t count)
{
    for (size_t option = 0; option < count; option++) {
        if (!specs[option].flag && !specs[option].value) {
            fprintf(stderr,
                    "hamn-image-tool: the following argument is required: --%s\n",
                    specs[option].name);
            return -1;
        }
    }
    return 0;
}

/* Decimal integer; out-of-range text maps to -1 for the caller's range check. */
static int parse_integer(const char *text, long long *out)
{
    const char *digits = text[0] == '-' || text[0] == '+' ? text + 1 : text;
    if (!*digits)
        return -1;
    for (const char *c = digits; *c; c++) {
        if (*c < '0' || *c > '9')
            return -1;
    }
    errno = 0;
    long long value = strtoll(text, NULL, 10);
    *out = errno == ERANGE ? -1 : value;
    return 0;
}

static int command_evidence(int argc, char **argv)
{
    struct image_error error;
    const char *prefix = "hamn baseline evidence: ";
    if (argc >= 2 && strcmp(argv[0], "check") == 0)
        return finish(evidence_check(argv[1], argv + 2, (size_t)(argc - 2),
                                     &error), prefix, &error);
    if (argc == 4 && strcmp(argv[0], "publish") == 0)
        return finish(evidence_publish(argv[1], argv[2], argv[3], NULL, &error),
                      prefix, &error);
    return usage(NULL);
}

static int command_verify_size(int argc, char **argv)
{
    struct option_spec specs[] = {
        { "baseline", 0, NULL }, { "candidate", 0, NULL },
        { "packages-before", 0, NULL }, { "packages-after", 0, NULL },
        { "report", 0, NULL }, { "budget", 0, NULL },
        { "base-sha256", 0, NULL }, { "source-revision", 0, NULL },
        { "review-only", 1, NULL },
    };
    const size_t count = sizeof(specs) / sizeof(specs[0]);
    if (parse_options(argc, argv, specs, count) != 0 ||
        require_options(specs, count) != 0)
        return usage(NULL);
    struct size_gate_options options = {
        .baseline = specs[0].value,
        .candidate = specs[1].value,
        .packages_before = specs[2].value,
        .packages_after = specs[3].value,
        .report = specs[4].value,
        .budget = specs[5].value,
        .base_sha256 = specs[6].value,
        .source_revision = specs[7].value,
        .review_only = specs[8].value != NULL,
    };
    struct image_error error;
    return finish(size_gate_run(&options, &error),
                  "hamn guest image size gate: ", &error);
}

static int command_verify_release_size(int argc, char **argv)
{
    if (argc != 3 && argc != 4)
        return usage(NULL);
    struct image_error error;
    return finish(size_release_verify(argv[0], argv[1], argv[2],
                                      argc == 4 ? argv[3] : NULL, &error),
                  "hamn guest image release size gate: ", &error);
}

static int command_verify_raw(int argc, char **argv)
{
    if (argc != 1)
        return usage(NULL);
    struct image_error error;
    return finish(raw_check(argv[0], &error), "", &error);
}

static int command_variations(int argc, char **argv)
{
    struct option_spec specs[] = {
        { "source-root", 0, NULL }, { "baseline", 0, NULL },
        { "size-report", 0, NULL }, { "output-directory", 0, NULL },
        { "seed", 0, NULL }, { "case-count", 0, NULL },
    };
    if (parse_options(argc, argv, specs, sizeof(specs) / sizeof(specs[0])) != 0 ||
        require_options(specs, 4) != 0)
        return usage(NULL);
    struct variations_options options = {
        .source_root = specs[0].value,
        .baseline = specs[1].value,
        .size_report = specs[2].value,
        .output_directory = specs[3].value,
        .seed = VARIATION_DEFAULT_SEED,
        .case_count = VARIATION_DEFAULT_CASES,
    };
    if ((specs[4].value && parse_integer(specs[4].value, &options.seed) != 0) ||
        (specs[5].value && parse_integer(specs[5].value,
                                         &options.case_count) != 0))
        return usage("--seed and --case-count must be decimal integers");
    struct image_error error;
    return finish(variations_run(&options, &error), "hamn image variations: ",
                  &error);
}

int main(int argc, char **argv)
{
    if (argc < 2)
        return usage(NULL);
    const char *command = argv[1];
    int rest = argc - 2;
    char **arguments = argv + 2;
    if (strcmp(command, "evidence") == 0)
        return command_evidence(rest, arguments);
    if (strcmp(command, "verify-raw") == 0)
        return command_verify_raw(rest, arguments);
    if (strcmp(command, "verify-size") == 0)
        return command_verify_size(rest, arguments);
    if (strcmp(command, "verify-release-size") == 0)
        return command_verify_release_size(rest, arguments);
    if (strcmp(command, "variations") == 0)
        return command_variations(rest, arguments);
    return usage(NULL);
}
