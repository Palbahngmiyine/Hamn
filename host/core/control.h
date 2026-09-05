#ifndef HAMN_CONTROL_H
#define HAMN_CONTROL_H

/* Call only in a fresh, single-threaded worker process. The caller owns the
 * returned UTF-8 JSON and must release it with hamn_control_free(). */
int hamn_control_query(const char *profile, char **result);
void hamn_control_free(char *result);
int hamn_control_start(const char *profile, unsigned cpus,
                       unsigned memory_gib, unsigned disk_gib);
int hamn_control_configure(const char *profile, unsigned cpus,
                           unsigned memory_gib, unsigned disk_gib, int create);
int hamn_control_stop(const char *profile);
int hamn_control_delete(const char *profile);

#endif
