#include "musicd.h"

static void (*log_callback)(int level, const char *);

static void lav_callback(void *av_class, int av_level, const char *fmt, va_list va_args) {
    (void)av_class;

    int level = 0;

    if (av_level >= AV_LOG_DEBUG) {
        return;
    } else if (av_level >= AV_LOG_VERBOSE) {
        level = LogLevelTrace;
    } else if (av_level >= AV_LOG_INFO) {
        level = LogLevelDebug;
    } else if (av_level >= AV_LOG_WARNING) {
        level = LogLevelWarn;
    } else {
        level = LogLevelError;
    }

    char buf[1024];
    vsnprintf(buf, sizeof(buf) - 1, fmt, va_args);

    log_callback(level, buf);
}

void musicd_log_setup(void (*callback)(int level, const char *)) {
    av_log_set_callback(lav_callback);
    log_callback = callback;
}

void lav_error(const char *msg, int lav_result) {
    char buf[1024];
    snprintf(buf, sizeof(buf) - 1, "%s: %s\n", msg, av_err2str(lav_result));
    log_callback(LogLevelError, buf);
}