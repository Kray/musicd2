#include "musicd.h"

static const char *get_metadata(
    const AVFormatContext *avctx,
    int stream_index,
    const char *key
) {
    const AVDictionaryEntry *entry = av_dict_get(
        avctx->streams[stream_index]->metadata,
        key, NULL, 0);

    if (!entry) {
        entry = av_dict_get(avctx->metadata, key, NULL, 0);
        if (!entry) {
            return NULL;
        }
    }

    return entry->value;
}

static char *copy_metadata(
    const AVFormatContext *avctx,
    int stream_index,
    const char *key
) {
    return av_strdup(get_metadata(avctx, stream_index, key));
}

static struct TrackInfo *try_get_track_info(
    const AVFormatContext *avctx,
    int stream_index,
    int track_index,
    const char *path);

static struct ImageInfo *try_get_image_info(
    const AVFormatContext *avctx,
    int stream_index,
    const char *path);

struct MediaInfo *media_info_from_path(const char *path) {
    const AVOutputFormat *fmt = av_guess_format(NULL, path, NULL);

    if (!fmt) {
        return NULL;
    }

    if (fmt->audio_codec == AV_CODEC_ID_NONE && fmt->video_codec == AV_CODEC_ID_NONE) {
        return NULL;
    }

    AVFormatContext *avctx = NULL;
    if (avformat_open_input(&avctx, path, NULL, NULL) < 0) {
        return NULL;
    }

    if (avctx->nb_streams < 1 || avctx->duration < 1) {
        if (avformat_find_stream_info(avctx, NULL) < 0) {
            avformat_close_input(&avctx);
            return NULL;
        }
    }

    struct MediaInfo *media_info = calloc(1, sizeof(struct MediaInfo));
    memset(media_info, 0, sizeof(struct MediaInfo));

    struct TrackInfo *track_cur = NULL;
    struct ImageInfo *image_cur = NULL;

    // av_dump_format(avctx, 0, NULL, 0);

    for (unsigned int i = 0; i < avctx->nb_streams; ++i) {
        struct TrackInfo *track_info = try_get_track_info(avctx, i, 0, path);
        if (track_info) {
            if (!track_cur) {
                media_info->tracks = track_info;
            } else {
                track_cur->next = track_info;
            }

            track_cur = track_info;

            continue;
        }

        struct ImageInfo *image_info = try_get_image_info(avctx, i, path);
        if (image_info) {
            if (!image_cur) {
                media_info->images = image_info;
            } else {
                image_cur->next = image_info;
            }

            image_cur = image_info;

            continue;
        }
    }

    avformat_close_input(&avctx);
    return media_info;
}

static char *extract_name_from_path(const char *path) {
    const char *start, *end;

    for (start = (char *)path + strlen(path); start > path && *(start - 1) != '/'; --start) { }
    for (end = start; *end != '.' && *end != '\0'; ++end) { }

    return av_strndup(start, end - start);
}

static struct TrackInfo *try_get_track_info(
    const AVFormatContext *avctx,
    int stream_index,
    int track_index,
    const char *path
) {
    const AVStream *stream = avctx->streams[stream_index];

    if (stream->codecpar->codec_type != AVMEDIA_TYPE_AUDIO) {
        return NULL;
    }

    double length = avctx->duration > 0
        ? avctx->duration / (double)AV_TIME_BASE
        : stream->duration * (double)av_q2d(stream->time_base);

    if (length <= 0) {
        return NULL;
    }

    struct TrackInfo *track_info = malloc(sizeof(struct TrackInfo));
    memset(track_info, 0, sizeof(struct TrackInfo));

    track_info->stream_index = stream_index;
    track_info->track_index = track_index;

    track_info->length = length;

    const char *tmp = get_metadata(avctx, stream_index, "track");
    if (tmp) {
        sscanf(tmp, "%d", &track_info->number);
    } else {
        track_info->number = track_index;
    }

    track_info->title = copy_metadata(avctx, stream_index, "title");
    if (!track_info->title) {
        track_info->title = copy_metadata(avctx, stream_index, "song");
    }
    if (!track_info->title) {
        track_info->title = extract_name_from_path(path);
    }

    track_info->artist = copy_metadata(avctx, stream_index, "artist");
    if (!track_info->artist) {
        copy_metadata(avctx, stream_index, "author");
    }

    track_info->album = copy_metadata(avctx, stream_index, "album");
    if (!track_info->album) {
        track_info->album = copy_metadata(avctx, stream_index, "game");
    }

    track_info->album_artist = copy_metadata(avctx, stream_index, "album_artist");
    if (!track_info->album_artist) {
        track_info->album_artist = copy_metadata(avctx, stream_index, "albumartist");
    }
    if (!track_info->album_artist) {
        track_info->album_artist = copy_metadata(avctx, stream_index, "album artist");
    }

    return track_info;
}

static struct ImageInfo *try_get_image_info(
    const AVFormatContext *avctx,
    int stream_index,
    const char *path
) {
    const AVStream *stream = avctx->streams[stream_index];

    if (!(stream->disposition & AV_DISPOSITION_ATTACHED_PIC)
        || stream->codecpar->codec_id != AV_CODEC_ID_MJPEG) {
        return NULL;
    }

    int width = stream->codecpar->width;
    int height = stream->codecpar->height;

    if (width <= 0 || height <= 0) {
        return NULL;
    }

    struct ImageInfo *image_info = malloc(sizeof(struct ImageInfo));
    memset(image_info, 0, sizeof(struct ImageInfo));

    image_info->stream_index = stream_index;

    image_info->description = copy_metadata(avctx, stream_index, "comment");
    if (!image_info->description) {
        image_info->description = extract_name_from_path(path);
    }

    image_info->width = width;
    image_info->height = height;

    return image_info;
}

//     // TODO multiple tracks in single file
//     // char *tmp;
//     // int track_count;
//     // tmp = copy_metadata(avctx, "tracks");
//     // if (tmp) {
//     //     sscanf(tmp, "%d", &track_count);
//     // } else {
//     //     track_count = 1;
//     // }

static void track_info_free(struct TrackInfo *track_info) {
    while (track_info) {
        free(track_info->title);
        free(track_info->artist);
        free(track_info->album);
        free(track_info->album_artist);

        struct TrackInfo *prev = track_info;
        track_info = track_info->next;
        free(prev);
    }
}

static void image_info_free(struct ImageInfo *image_info) {
    while (image_info) {
        free(image_info->description);

        struct ImageInfo *prev = image_info;
        image_info = image_info->next;
        free(prev);
    }
}

void media_info_free(struct MediaInfo *media_info) {
    track_info_free(media_info->tracks);
    image_info_free(media_info->images);
    free(media_info);
}

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
