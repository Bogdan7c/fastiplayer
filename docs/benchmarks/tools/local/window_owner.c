/* Проверка XWayland ownership по XRes: VLC не выставляет _NET_WM_PID.
 * Ответ сервера связывает X resource с локальным PID без угадывания по WM_CLASS. */
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <xcb/res.h>
#include <xcb/xcb.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    char *end = NULL;
    errno = 0;
    unsigned long parsed = strtoul(argv[1], &end, 0);
    if (errno || !end || *end || parsed > UINT32_MAX) return 2;
    xcb_connection_t *connection = xcb_connect(NULL, NULL);
    if (xcb_connection_has_error(connection)) {
        xcb_disconnect(connection);
        return 3;
    }
    xcb_res_client_id_spec_t specification = {
        .client = (uint32_t)parsed, .mask = XCB_RES_CLIENT_ID_MASK_LOCAL_CLIENT_PID
    };
    xcb_generic_error_t *error = NULL;
    xcb_res_query_client_ids_reply_t *reply = xcb_res_query_client_ids_reply(
        connection, xcb_res_query_client_ids(connection, 1, &specification), &error);
    int result = 4;
    if (reply && !error) {
        xcb_res_client_id_value_iterator_t values = xcb_res_query_client_ids_ids_iterator(reply);
        for (; values.rem; xcb_res_client_id_value_next(&values)) {
            if ((values.data->spec.mask & XCB_RES_CLIENT_ID_MASK_LOCAL_CLIENT_PID) &&
                xcb_res_client_id_value_value_length(values.data) == 1) {
                uint32_t pid = *xcb_res_client_id_value_value(values.data);
                xcb_get_geometry_reply_t *geometry = xcb_get_geometry_reply(
                    connection, xcb_get_geometry(connection, (xcb_drawable_t)parsed), NULL);
                if (geometry) {
                    printf("%u %u %u\n", pid, geometry->width, geometry->height);
                    free(geometry);
                    result = 0;
                }
                break;
            }
        }
    }
    free(error);
    free(reply);
    xcb_disconnect(connection);
    return result;
}
