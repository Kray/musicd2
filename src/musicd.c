#include "musicd.h"

void musicd_init() {
    //av_log_set_level(AV_LOG_QUIET);
}

void lav_error(const char *msg, int lav_result) {
    fprintf(stderr, "%s: %d\n", msg, lav_result);
}