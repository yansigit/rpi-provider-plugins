#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
/* Only the selected fixture descriptor fails; unrelated descriptors use libc. */
int fsync(int fd) {
    const char *match = getenv("RPI_FSYNC_FAILURE_MATCH");
    char descriptor[64], path[4096];
    snprintf(descriptor, sizeof(descriptor), "/proc/self/fd/%d", fd);
    ssize_t length = readlink(descriptor, path, sizeof(path) - 1);
    if (match && length >= 0) {
        path[length] = '\0';
        const char *kind = getenv("RPI_FSYNC_FAILURE_KIND");
        struct stat st;
        int directory = kind && strcmp(kind, "directory") == 0;
        if (strstr(path, match) && (!directory || (fstat(fd, &st) == 0 && S_ISDIR(st.st_mode)))) {
            errno = EIO; return -1;
        }
    }
    int (*real_fsync)(int) = dlsym(RTLD_NEXT, "fsync");
    if (!real_fsync) { errno = ENOSYS; return -1; }
    return real_fsync(fd);
}
