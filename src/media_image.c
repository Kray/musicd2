#include "musicd.h"

int media_image_data_read(
    const char *path,
    int32_t stream_index,
    uint8_t **out_data,
    size_t *out_len
) {
    int result;

    AVFormatContext *in_ctx = NULL;
    result = avformat_open_input(&in_ctx, path, NULL, NULL);
    if (result < 0) {
        lav_error("avformat_open_input", result);
        goto fail;
    }

    result = avformat_find_stream_info(in_ctx, NULL);
    if (result < 0) {
        lav_error("avformat_find_stream_info", result);
        goto fail;
    }

    if (in_ctx->nb_streams <= (uint32_t)stream_index) {
        lav_error("image stream doesn't exist", 0);
        goto fail;
    }

    AVPacket packet = { .data = NULL, .size = 0 };

    while (1) {
        result = av_read_frame(in_ctx, &packet);
        if (result < 0) {
            if (result == AVERROR_EOF) {
                // End of file
                break;
            }

            lav_error("av_read_frame", result);
            break;
        }

        if (packet.stream_index != stream_index) {
            continue;
        }

        *out_data = malloc(packet.size);
        memcpy(*out_data, packet.data, packet.size);

        *out_len = packet.size;

        avformat_close_input(&in_ctx);
        return 1;
    }

fail:
    avformat_close_input(&in_ctx);
    return 0;
}

void media_image_data_free(uint8_t *data) {
    free(data);
}