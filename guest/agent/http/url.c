#include "http/url.h"

static int hex_value(char value)
{
    if (value >= '0' && value <= '9')
        return value - '0';
    if (value >= 'a' && value <= 'f')
        return value - 'a' + 10;
    if (value >= 'A' && value <= 'F')
        return value - 'A' + 10;
    return -1;
}

int http_url_decode(char *value)
{
    if (!value)
        return -1;
    char *read = value;
    char *write = value;
    while (*read) {
        if (*read == '%') {
            if (!read[1] || !read[2])
                return -1;
            int high = hex_value(read[1]);
            int low = hex_value(read[2]);
            if (high < 0 || low < 0 || (high == 0 && low == 0))
                return -1;
            *write++ = (char)((high << 4) | low);
            read += 3;
        } else {
            *write++ = *read == '+' ? ' ' : *read;
            read++;
        }
    }
    *write = '\0';
    return 0;
}
