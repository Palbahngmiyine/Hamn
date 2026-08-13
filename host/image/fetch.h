#ifndef HAMN_FETCH_H
#define HAMN_FETCH_H

#include <stddef.h>

/*
 * Signed release/update가 ~/.hamn/cache에 설치하고 선택한 preconfigured
 * Ubuntu 24.04 arm64 image만 반환한다. Stock cloud image download fallback은
 * 허용하지 않는다. 0=성공(img_out 채움), -1=실패.
 */
int fetch_image_ensure(char *img_out, size_t cap);

#endif
