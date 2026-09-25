#ifndef HOSWM_H
#define HOSWM_H
/* HOSWM ABI v1, Linux. Define HOSWM_IMPLEMENTATION in exactly one C file.
 * Calls connect to the current user's HOSWM session. Coordinates are content
 * pixels; colors are 0xAARRGGBB. On failure return -1 (or window ID 0), set errno.
 * Server validation failures use EPROTO. Strings are UTF-8, NUL-terminated.
 */
#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
#define HOSWM_ABI_VERSION 1
#define HOS_LABEL 1
#define HOS_BUTTON 2
#define HOS_TEXTBOX 3
#define HOS_EVENT_NONE 0
#define HOS_EVENT_CLICK 1
#define HOS_EVENT_CHANGE 2
#define HOS_EVENT_SUBMIT 3
#define HOS_EVENT_RESIZE 4 /* control = content width; text = decimal height */
#define HOS_EVENT_POINTER 5 /* text = "x y" content coordinates, left press */
#define HOS_EVENT_KEY 6 /* text contains key bytes (see text_length) */
#define HOS_EVENT_CLOSED 7
#define HOS_EVENT_RAW_POINTER 8 /* control=1 button, 2 right button, 0 motion; text="x y action" */
#define HOS_EVENT_CLOSE_REQUEST 9 /* defer/deny a user window close request */
#define HOS_EVENT_WHEEL 10 /* control = signed wheel delta (positive is up); text = "x y axis"; axis 0 vertical, 1 horizontal */
#define HOS_EVENT_MENU 11 /* menu bar item chosen: control = item ID, text = its label */
#define HOS_MENU_DISABLED 1u /* item is shown greyed out and cannot be chosen */
#define HOS_MENU_SEPARATOR 2u /* horizontal rule; id and label are ignored */
#define HOS_MENU_CHECKED 4u /* item is marked as active */
/* Message box button sets, severities and answers. */
#define HOS_BUTTONS_OK 0u
#define HOS_BUTTONS_OK_CANCEL 1u
#define HOS_BUTTONS_YES_NO 2u
#define HOS_BUTTONS_YES_NO_CANCEL 3u
#define HOS_INFO 0u
#define HOS_WARNING 1u
#define HOS_ERROR 2u
#define HOS_QUESTION 3u
#define HOS_ANSWER_CLOSED 0 /* the window was closed without an answer */
#define HOS_ANSWER_OK 1
#define HOS_ANSWER_CANCEL 2
#define HOS_ANSWER_YES 3
#define HOS_ANSWER_NO 4
#define HOS_WINDOW_RAW_INPUT 1u /* operation 12: deliver keyboard and pointer events */
#define HOS_WINDOW_DEFER_CLOSE 2u /* operation 12: deliver close requests as events */
#define HOS_WINDOW_PROTECT_EXIT 4u /* operation 12: inhibit session exit while critical work runs */
#define HOS_WINDOW_RESIZABLE 8u /* operation 12: let the user drag the window edges; the new size arrives as HOS_EVENT_RESIZE */

typedef uint32_t HosWindow;
typedef struct { uint32_t kind, control, text_length; char text[1025]; } HosEvent;
/* Menu bar entries. The window manager draws the menus of the focused window
 * across the top of the screen; choosing an item queues HOS_EVENT_MENU. */
typedef struct { uint32_t id, flags; const char *label, *shortcut; } HosMenuItem;
typedef struct { const char *title; const HosMenuItem *items; uint32_t item_count; } HosMenu;
int hos_session_info(uint32_t *version, uint32_t *width, uint32_t *height);
HosWindow hos_window_create(const char *title, uint32_t width, uint32_t height, uint32_t color);
HosWindow hos_gui_window_create(const char *title, uint32_t width, uint32_t height, uint32_t color);
HosWindow hos_message_box(const char *title, const char *message, uint32_t color);
int hos_window_close(HosWindow window);
int hos_window_set_flags(HosWindow window, uint32_t flags);
int hos_clipboard_set(const char *text);
int hos_clipboard_get(char *text, size_t capacity);
int hos_window_size(HosWindow window, uint32_t *width, uint32_t *height, uint32_t *minimized);
int hos_window_present(HosWindow window, uint32_t width, uint32_t height, const uint32_t *argb);
int hos_window_poll(HosWindow window, HosEvent *event); /* 1 event, 0 empty, -1 error */
/* Show a notification in the session corner; 0 milliseconds uses the
 * configured default. It is logged to ~/.hoswm/toastdb once it disappears. */
int hos_toast(const char *text, uint32_t color, uint32_t milliseconds);
/* Message box with answer buttons. hos_message_box_ask blocks until the user
 * answers and returns HOS_ANSWER_*, or -1 on error. hos_message_box_open
 * returns the window so a program can keep working; its answer arrives as
 * HOS_EVENT_CLICK with the HOS_ANSWER_* value in control, and the program
 * closes the window. A closed window reports HOS_EVENT_CLOSED. */
HosWindow hos_message_box_open(const char *title, const char *text, uint32_t buttons, uint32_t severity);
int hos_message_box_ask(const char *title, const char *text, uint32_t buttons, uint32_t severity);
/* Replace this window's menus; count 0 removes them. At most 8 menus of 32
 * items each; titles are 32 bytes, labels 48 and shortcut hints 16. */
int hos_window_set_menus(HosWindow window, const HosMenu *menus, uint32_t count);
/* Opt-in selection for labels/textboxes; existing constructors default to false.
 * Buttons reject selectable=true. Requires the opcode 11 ABI v1 extension. */
int hos_gui_control_ex(HosWindow window, uint32_t id, uint32_t kind, uint32_t x, uint32_t y, uint32_t width, uint32_t height, const char *text, bool selectable);
int hos_gui_label_ex(HosWindow window, uint32_t id, uint32_t x, uint32_t y, uint32_t width, uint32_t height, const char *text, bool selectable);
int hos_gui_textbox_ex(HosWindow window, uint32_t id, uint32_t x, uint32_t y, uint32_t width, uint32_t height, const char *text, bool selectable);
int hos_gui_control(HosWindow window, uint32_t id, uint32_t kind, uint32_t x, uint32_t y, uint32_t width, uint32_t height, const char *text);
int hos_gui_label(HosWindow window, uint32_t id, uint32_t x, uint32_t y, uint32_t width, uint32_t height, const char *text);
int hos_gui_button(HosWindow window, uint32_t id, uint32_t x, uint32_t y, uint32_t width, uint32_t height, const char *text);
int hos_gui_textbox(HosWindow window, uint32_t id, uint32_t x, uint32_t y, uint32_t width, uint32_t height, const char *text);
int hos_gui_set_text(HosWindow window, uint32_t id, const char *text);
/* Removes the control, its focus/press state and queued control events. */
int hos_gui_remove_control(HosWindow window, uint32_t id);
int hos_gui_get_text(HosWindow window, uint32_t id, char *text, size_t capacity);
/* Sound. Audio does not travel through the window server: these calls reach
 * hos-soundd through /run/hos, which mixes every application's stream, so
 * several windows can play at the same time. Samples are interleaved signed
 * 16-bit; the service resamples to whatever the card is running at. */
typedef struct HosSound HosSound;
/* Open a playback stream. name appears in "hosctl sound streams". */
HosSound *hos_sound_open(uint32_t rate, uint32_t channels, const char *name);
/* Write count samples (frames * channels). Blocks while the mixer catches up. */
int hos_sound_write(HosSound *sound, const int16_t *samples, size_t count);
void hos_sound_close(HosSound *sound);
/* Ask the service to play a WAV file; returns once playback has started. */
int hos_sound_play_file(const char *path);
/* System volume in percent and mute state; either pointer may be NULL. */
int hos_sound_volume(uint32_t *percent, uint32_t *muted);
int hos_sound_set_volume(uint32_t percent);
int hos_sound_set_muted(bool muted);
#ifdef __cplusplus
}
#endif
#ifdef HOSWM_IMPLEMENTATION
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/time.h>
#include <poll.h>
static void hos_put(uint8_t *p, uint32_t n) { p[0]=(uint8_t)n; p[1]=(uint8_t)(n>>8); p[2]=(uint8_t)(n>>16); p[3]=(uint8_t)(n>>24); }
static uint32_t hos_get(const uint8_t *p) { return (uint32_t)p[0]|(uint32_t)p[1]<<8|(uint32_t)p[2]<<16|(uint32_t)p[3]<<24; }
static int hos_transfer(int fd, void *bytes, size_t size, int writing) {
    uint8_t *p=(uint8_t *)bytes;
    while (size) { ssize_t n=writing?send(fd,p,size,MSG_NOSIGNAL):recv(fd,p,size,0); if(n<0&&errno==EINTR) continue; if(n<=0) {if(n==0) errno=ECONNRESET;return -1;} p+=n;size-=(size_t)n; } return 0;
}
static int hos_call(const uint8_t *request, size_t size, uint8_t *reply, size_t capacity, size_t *received) {
    int fd=socket(AF_UNIX,SOCK_STREAM|SOCK_CLOEXEC,0);if(fd<0)return -1;
    struct timeval timeout={3,0};struct sockaddr_un addr;uint8_t header[8];int result=-1;
    memset(&addr,0,sizeof(addr));addr.sun_family=AF_UNIX;
    const char *socket_path=getenv("HOSWM_SOCKET");
    if(socket_path) {
        if(strlen(socket_path)>=sizeof(addr.sun_path)) {errno=ENAMETOOLONG;goto done;}
        memcpy(addr.sun_path,socket_path,strlen(socket_path)+1);
    } else snprintf(addr.sun_path,sizeof(addr.sun_path),"/tmp/hoswm-%lu/session.sock",(unsigned long)geteuid());
    if(setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&timeout,sizeof(timeout))<0||setsockopt(fd,SOL_SOCKET,SO_SNDTIMEO,&timeout,sizeof(timeout))<0)goto done;
    if(connect(fd,(struct sockaddr *)&addr,sizeof(addr))<0)goto done;
    hos_put(header,(uint32_t)size);
    if(hos_transfer(fd,header,4,1)<0||hos_transfer(fd,(void *)request,size,1)<0||hos_transfer(fd,header,8,0)<0)goto done;
    uint32_t length=hos_get(header);
    if(length<4||hos_get(header+4)!=0) {errno=EPROTO;goto done;}
    if(length-4>capacity) {errno=EMSGSIZE;goto done;}
    if(hos_transfer(fd,reply,length-4,0)<0)goto done;
    if(received)*received=length-4;
    result=0;
 done: {int error=errno;close(fd);errno=error;}return result;
}
static int hos_string_size(const char *s, size_t max, size_t *length) {
    if(!s) {errno=EINVAL;return -1;} *length=strlen(s);if(*length>max) {errno=EMSGSIZE;return -1;}return 0;
}
int hos_session_info(uint32_t *version,uint32_t *width,uint32_t *height) {
    uint8_t req[4],reply[12];size_t got;hos_put(req,0);
    if(hos_call(req,4,reply,sizeof(reply),&got)<0)return -1;
    if(got!=12) {errno=EPROTO;return -1;}
    if(version)*version=hos_get(reply);
    if(width)*width=hos_get(reply+4);
    if(height)*height=hos_get(reply+8);
    return 0;
}
int hos_gui_remove_control(HosWindow window,uint32_t id) {
    uint8_t req[12];hos_put(req,10);hos_put(req+4,window);hos_put(req+8,id);
    return hos_call(req,12,NULL,0,NULL);
}
int hos_window_set_flags(HosWindow window,uint32_t flags) {
    if(flags&~15u) {errno=EINVAL;return -1;}
    uint8_t req[12];hos_put(req,12);hos_put(req+4,window);hos_put(req+8,flags);
    return hos_call(req,sizeof(req),NULL,0,NULL);
}
int hos_clipboard_set(const char *text) {
    size_t n;if(hos_string_size(text,65536,&n)<0)return -1;
    uint8_t *req=(uint8_t *)malloc(8+n);if(!req)return -1;
    hos_put(req,13);hos_put(req+4,(uint32_t)n);memcpy(req+8,text,n);
    int result=hos_call(req,8+n,NULL,0,NULL);free(req);return result;
}
int hos_clipboard_get(char *text,size_t capacity) {
    if(!text||!capacity){errno=EINVAL;return -1;}
    uint8_t req[4],*reply=(uint8_t *)malloc(65540);size_t got;
    if(!reply)return -1;
    hos_put(req,14);
    if(hos_call(req,sizeof(req),reply,65540,&got)<0){free(reply);return -1;}
    if(got<4||got!=4+hos_get(reply)){free(reply);errno=EPROTO;return -1;}
    size_t n=got-4;if(n>=capacity){free(reply);errno=EMSGSIZE;return -1;}
    memcpy(text,reply+4,n);text[n]=0;free(reply);return (int)n;
}
HosWindow hos_window_create(const char *title,uint32_t width,uint32_t height,uint32_t color) {
    size_t n,got;uint8_t req[148],reply[4];if(hos_string_size(title,128,&n)<0)return 0;
    hos_put(req,1);hos_put(req+4,width);hos_put(req+8,height);hos_put(req+12,color);hos_put(req+16,(uint32_t)n);memcpy(req+20,title,n);
    if(hos_call(req,20+n,reply,4,&got)<0)return 0;
    if(got!=4) {errno=EPROTO;return 0;}return hos_get(reply);
}
HosWindow hos_gui_window_create(const char *title,uint32_t width,uint32_t height,uint32_t color) {return hos_window_create(title,width,height,color);}
HosWindow hos_message_box(const char *title,const char *message,uint32_t color) {
    size_t n,m,got;uint8_t req[2192],reply[4];if(hos_string_size(title,128,&n)<0||hos_string_size(message,2048,&m)<0)return 0;
    hos_put(req,2);hos_put(req+4,color);hos_put(req+8,(uint32_t)n);memcpy(req+12,title,n);hos_put(req+12+n,(uint32_t)m);memcpy(req+16+n,message,m);
    if(hos_call(req,16+n+m,reply,4,&got)<0)return 0;
    if(got!=4) {errno=EPROTO;return 0;}return hos_get(reply);
}
int hos_window_close(HosWindow window) {uint8_t req[8];hos_put(req,4);hos_put(req+4,window);return hos_call(req,8,NULL,0,NULL);}
HosWindow hos_message_box_open(const char *title,const char *text,uint32_t buttons,uint32_t severity) {
    size_t n,m,got;uint8_t req[2196],reply[4];
    if(hos_string_size(title,128,&n)<0||hos_string_size(text,2048,&m)<0)return 0;
    hos_put(req,17);hos_put(req+4,buttons);hos_put(req+8,severity);hos_put(req+12,(uint32_t)n);memcpy(req+16,title,n);
    hos_put(req+16+n,(uint32_t)m);memcpy(req+20+n,text,m);
    if(hos_call(req,20+n+m,reply,4,&got)<0)return 0;
    if(got!=4) {errno=EPROTO;return 0;}return hos_get(reply);
}
int hos_message_box_ask(const char *title,const char *text,uint32_t buttons,uint32_t severity) {
    HosWindow window=hos_message_box_open(title,text,buttons,severity);
    if(!window)return -1;
    for(;;) {
        HosEvent event;int result=hos_window_poll(window,&event);
        if(result<0) {int error=errno;hos_window_close(window);errno=error;return -1;}
        if(result&&event.kind==HOS_EVENT_CLICK) {hos_window_close(window);return (int)event.control;}
        if(result&&event.kind==HOS_EVENT_CLOSED)return HOS_ANSWER_CLOSED;
        poll(NULL,0,25); /* sleep without depending on feature-test macros */
    }
}
int hos_toast(const char *text,uint32_t color,uint32_t milliseconds) {
    size_t n;if(hos_string_size(text,512,&n)<0)return -1;
    uint8_t req[528];hos_put(req,15);hos_put(req+4,color);hos_put(req+8,milliseconds);hos_put(req+12,(uint32_t)n);
    memcpy(req+16,text,n);return hos_call(req,16+n,NULL,0,NULL);
}
int hos_window_set_menus(HosWindow window,const HosMenu *menus,uint32_t count) {
    if(count>8||(count&&!menus)) {errno=EINVAL;return -1;}
    /* Worst case: 8 menus of 32 items with maximum strings. */
    size_t capacity=12+(size_t)count*(36+32*(16+48+16)),size=12;
    uint8_t *req=(uint8_t *)malloc(capacity);if(!req)return -1;
    hos_put(req,16);hos_put(req+4,window);hos_put(req+8,count);
    for(uint32_t i=0;i<count;i++) {
        size_t title;
        if(hos_string_size(menus[i].title,32,&title)<0) {free(req);return -1;}
        if(menus[i].item_count>32||(menus[i].item_count&&!menus[i].items)) {free(req);errno=EINVAL;return -1;}
        hos_put(req+size,(uint32_t)title);memcpy(req+size+4,menus[i].title,title);size+=4+title;
        hos_put(req+size,menus[i].item_count);size+=4;
        for(uint32_t j=0;j<menus[i].item_count;j++) {
            const HosMenuItem *item=&menus[i].items[j];
            const char *label=item->label?item->label:"",*shortcut=item->shortcut?item->shortcut:"";
            size_t label_size,shortcut_size;
            if(item->flags&~7u) {free(req);errno=EINVAL;return -1;}
            if(hos_string_size(label,48,&label_size)<0||hos_string_size(shortcut,16,&shortcut_size)<0) {free(req);return -1;}
            hos_put(req+size,item->id);hos_put(req+size+4,item->flags);hos_put(req+size+8,(uint32_t)label_size);
            memcpy(req+size+12,label,label_size);size+=12+label_size;
            hos_put(req+size,(uint32_t)shortcut_size);memcpy(req+size+4,shortcut,shortcut_size);size+=4+shortcut_size;
        }
    }
    int result=hos_call(req,size,NULL,0,NULL);free(req);return result;
}
int hos_window_size(HosWindow window,uint32_t *width,uint32_t *height,uint32_t *minimized) {
    uint8_t req[8],reply[12];size_t got;hos_put(req,8);hos_put(req+4,window);if(hos_call(req,8,reply,12,&got)<0)return -1;
    if(got!=12) {errno=EPROTO;return -1;}if(width)*width=hos_get(reply);if(height)*height=hos_get(reply+4);if(minimized)*minimized=hos_get(reply+8);return 0;
}
int hos_window_present(HosWindow window,uint32_t width,uint32_t height,const uint32_t *argb) {
    if(!argb||width>796||height>500||width==0||height==0) {errno=EINVAL;return -1;}
    size_t count=(size_t)width*height;uint8_t *req=(uint8_t *)malloc(16+count*4);if(!req)return -1;
    hos_put(req,3);hos_put(req+4,window);hos_put(req+8,width);hos_put(req+12,height);for(size_t i=0;i<count;i++)hos_put(req+16+i*4,argb[i]);
    int result=hos_call(req,16+count*4,NULL,0,NULL);free(req);return result;
}
int hos_window_poll(HosWindow window,HosEvent *event) {
    uint8_t req[8],reply[1036];size_t got;if(!event) {errno=EINVAL;return -1;}hos_put(req,5);hos_put(req+4,window);
    if(hos_call(req,8,reply,sizeof(reply),&got)<0)return -1;
    if(got<12||hos_get(reply+8)>1024||got!=12+hos_get(reply+8)) {errno=EPROTO;return -1;}
    event->kind=hos_get(reply);event->control=hos_get(reply+4);event->text_length=hos_get(reply+8);memcpy(event->text,reply+12,event->text_length);event->text[event->text_length]=0;return event->kind?1:0;
}
int hos_gui_control(HosWindow window,uint32_t id,uint32_t kind,uint32_t x,uint32_t y,uint32_t width,uint32_t height,const char *text) {
    size_t n;uint8_t req[1060];if(hos_string_size(text,1024,&n)<0)return -1;uint32_t fields[]={6,window,id,kind,x,y,width,height,(uint32_t)n};
    for(size_t i=0;i<9;i++) {hos_put(req+i*4,fields[i]);}
    memcpy(req+36,text,n);return hos_call(req,36+n,NULL,0,NULL);
}
int hos_gui_control_ex(HosWindow window,uint32_t id,uint32_t kind,uint32_t x,uint32_t y,uint32_t width,uint32_t height,const char *text,bool selectable) {
    if(!selectable)return hos_gui_control(window,id,kind,x,y,width,height,text);
    size_t n;uint8_t req[1064];if(hos_string_size(text,1024,&n)<0)return -1;
    uint32_t fields[]={11,window,id,kind,x,y,width,height,1,(uint32_t)n};
    for(size_t i=0;i<10;i++)hos_put(req+i*4,fields[i]);
    memcpy(req+40,text,n);return hos_call(req,40+n,NULL,0,NULL);
}
int hos_gui_label_ex(HosWindow w,uint32_t id,uint32_t x,uint32_t y,uint32_t width,uint32_t height,const char *text,bool selectable) {return hos_gui_control_ex(w,id,HOS_LABEL,x,y,width,height,text,selectable);}
int hos_gui_textbox_ex(HosWindow w,uint32_t id,uint32_t x,uint32_t y,uint32_t width,uint32_t height,const char *text,bool selectable) {return hos_gui_control_ex(w,id,HOS_TEXTBOX,x,y,width,height,text,selectable);}
int hos_gui_label(HosWindow w,uint32_t id,uint32_t x,uint32_t y,uint32_t width,uint32_t height,const char *text) {return hos_gui_control(w,id,HOS_LABEL,x,y,width,height,text);}
int hos_gui_button(HosWindow w,uint32_t id,uint32_t x,uint32_t y,uint32_t width,uint32_t height,const char *text) {return hos_gui_control(w,id,HOS_BUTTON,x,y,width,height,text);}
int hos_gui_textbox(HosWindow w,uint32_t id,uint32_t x,uint32_t y,uint32_t width,uint32_t height,const char *text) {return hos_gui_control(w,id,HOS_TEXTBOX,x,y,width,height,text);}
int hos_gui_set_text(HosWindow window,uint32_t id,const char *text) {
    size_t n;uint8_t req[1040];if(hos_string_size(text,1024,&n)<0)return -1;hos_put(req,7);hos_put(req+4,window);hos_put(req+8,id);hos_put(req+12,(uint32_t)n);memcpy(req+16,text,n);return hos_call(req,16+n,NULL,0,NULL);
}
int hos_gui_get_text(HosWindow window,uint32_t id,char *text,size_t capacity) {
    size_t got;uint8_t req[12],reply[1028];if(!text||!capacity) {errno=EINVAL;return -1;}hos_put(req,9);hos_put(req+4,window);hos_put(req+8,id);
    if(hos_call(req,12,reply,sizeof(reply),&got)<0)return -1;
    if(got<4||got!=4+hos_get(reply)) {errno=EPROTO;return -1;}size_t n=got-4;if(n>=capacity) {errno=EMSGSIZE;return -1;}memcpy(text,reply+4,n);text[n]=0;return 0;
}
/* --- Sound ---------------------------------------------------------------
 * hos-soundd listens on two sockets in /run/hos: a line protocol for control
 * (soundd.sock) and a raw PCM socket for playback (sound.pcm). HOS_RUN_DIR
 * moves both, which is what the tests use. */
struct HosSound { int fd; };
static int hos_run_path(char *out, size_t capacity, const char *name) {
    const char *dir=getenv("HOS_RUN_DIR");
    int n=snprintf(out,capacity,"%s/%s",dir&&*dir?dir:"/run/hos",name);
    if(n<0||(size_t)n>=capacity) {errno=ENAMETOOLONG;return -1;}
    return 0;
}
static int hos_run_connect(const char *name) {
    struct sockaddr_un addr;memset(&addr,0,sizeof(addr));addr.sun_family=AF_UNIX;
    if(hos_run_path(addr.sun_path,sizeof(addr.sun_path),name)<0)return -1;
    int fd=socket(AF_UNIX,SOCK_STREAM|SOCK_CLOEXEC,0);if(fd<0)return -1;
    struct timeval timeout={5,0};
    if(setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&timeout,sizeof(timeout))<0||
       setsockopt(fd,SOL_SOCKET,SO_SNDTIMEO,&timeout,sizeof(timeout))<0||
       connect(fd,(struct sockaddr *)&addr,sizeof(addr))<0) {
        int error=errno;close(fd);errno=error;return -1;
    }
    return fd;
}
/* One request on a service socket. The reply line is copied without its
 * leading '+'; a '-' line sets errno to EPROTO and returns -1. */
static int hos_service_call(const char *service,const char *request,char *reply,size_t capacity) {
    int fd=hos_run_connect(service);if(fd<0)return -1;
    size_t size=strlen(request);char line[1024];size_t used=0;int result=-1,record=0;
    if(reply&&capacity)reply[0]=0;
    if(hos_transfer(fd,(void *)request,size,1)<0||hos_transfer(fd,(void *)"\n",1,1)<0)goto done;
    for(;;) {
        char c;ssize_t n=recv(fd,&c,1,0);
        if(n<0&&errno==EINTR)continue;
        if(n<=0) {if(n==0)errno=ECONNRESET;goto done;}
        if(c!='\n') {if(used+1<sizeof(line))line[used++]=c;continue;}
        line[used]=0;used=0;
        if(line[0]=='-') {errno=EPROTO;goto done;}
        /* The first record is the answer; the closing line is only used
         * when the service sent no records at all. */
        if((line[0]=='='&&!record)||(line[0]=='+'&&!record)) {
            if(reply&&capacity) {size_t n2=strlen(line+1);if(n2>=capacity)n2=capacity-1;memcpy(reply,line+1,n2);reply[n2]=0;}
            if(line[0]=='=')record=1;
        }
        if(line[0]=='+') {result=0;goto done;}
    }
 done: {int error=errno;close(fd);errno=error;}return result;
}
/* Find "key=value" in a record line and return the value's start. */
static const char *hos_field(const char *record,const char *key) {
    size_t n=strlen(key);
    for(const char *p=record;p&&*p;) {
        if(!strncmp(p,key,n)&&p[n]=='=')return p+n+1;
        p=strchr(p,' ');if(p)p++;
    }
    return NULL;
}
HosSound *hos_sound_open(uint32_t rate,uint32_t channels,const char *name) {
    if(rate<4000||rate>192000||channels<1||channels>8) {errno=EINVAL;return NULL;}
    size_t n=name?strlen(name):0;if(n>64)n=64;
    int fd=hos_run_connect("sound.pcm");if(fd<0)return NULL;
    uint8_t header[32];memcpy(header,"HOSPCM\0\0",8);
    hos_put(header+8,1);hos_put(header+12,rate);hos_put(header+16,channels);
    hos_put(header+20,0);hos_put(header+24,0);hos_put(header+28,(uint32_t)n);
    if(hos_transfer(fd,header,sizeof(header),1)<0||(n&&hos_transfer(fd,(void *)name,n,1)<0)) {
        int error=errno;close(fd);errno=error;return NULL;
    }
    HosSound *sound=(HosSound *)malloc(sizeof(HosSound));
    if(!sound) {int error=errno;close(fd);errno=error;return NULL;}
    sound->fd=fd;return sound;
}
int hos_sound_write(HosSound *sound,const int16_t *samples,size_t count) {
    if(!sound||(!samples&&count)) {errno=EINVAL;return -1;}
    uint8_t chunk[2048];
    while(count) {
        size_t batch=count<sizeof(chunk)/2?count:sizeof(chunk)/2;
        for(size_t i=0;i<batch;i++) {
            uint16_t value=(uint16_t)samples[i];
            chunk[i*2]=(uint8_t)value;chunk[i*2+1]=(uint8_t)(value>>8);
        }
        if(hos_transfer(sound->fd,chunk,batch*2,1)<0)return -1;
        samples+=batch;count-=batch;
    }
    return 0;
}
void hos_sound_close(HosSound *sound) {if(sound) {close(sound->fd);free(sound);}}
int hos_sound_play_file(const char *path) {
    if(!path||!*path) {errno=EINVAL;return -1;}
    char request[1100];size_t used=5;
    if(strlen(path)>512) {errno=ENAMETOOLONG;return -1;}
    memcpy(request,"PLAY ",5);
    for(const char *p=path;*p;p++) { /* the line protocol escapes spaces */
        if(*p==' ') {request[used++]='\\';request[used++]='s';}
        else if(*p=='\\') {request[used++]='\\';request[used++]='\\';}
        else request[used++]=*p;
    }
    request[used]=0;
    return hos_service_call("soundd.sock",request,NULL,0);
}
int hos_sound_volume(uint32_t *percent,uint32_t *muted) {
    char reply[256];
    if(hos_service_call("soundd.sock","VOLUME",reply,sizeof(reply))<0)return -1;
    const char *value=hos_field(reply,"volume");
    if(percent)*percent=value?(uint32_t)strtoul(value,NULL,10):0;
    const char *state=hos_field(reply,"muted");
    if(muted)*muted=state&&!strncmp(state,"yes",3);
    return 0;
}
int hos_sound_set_volume(uint32_t percent) {
    char request[32];snprintf(request,sizeof(request),"VOLUME %u",percent>100?100u:percent);
    return hos_service_call("soundd.sock",request,NULL,0);
}
int hos_sound_set_muted(bool muted) {
    return hos_service_call("soundd.sock",muted?"MUTE ON":"MUTE OFF",NULL,0);
}
#endif /* HOSWM_IMPLEMENTATION */
#endif /* HOSWM_H */
