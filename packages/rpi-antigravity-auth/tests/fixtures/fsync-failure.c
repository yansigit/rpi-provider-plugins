#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
/* Only the fixture's staged file fails; every unrelated descriptor uses libc. */
int fsync(int fd) {
    const char *match = getenv("RPI_FSYNC_FAILURE_MATCH");
    char descriptor[64], path[4096];
    snprintf(descriptor, sizeof(descriptor), "/proc/self/fd/%d", fd);
    ssize_t length = readlink(descriptor, path, sizeof(path) - 1);
    if (match && length >= 0) {
        path[length] = '\0';
        if (strstr(path, match)) { errno = EIO; return -1; }
    }
    int (*real_fsync)(int) = dlsym(RTLD_NEXT, "fsync");
    if (!real_fsync) { errno = ENOSYS; return -1; }
    return real_fsync(fd);
}
