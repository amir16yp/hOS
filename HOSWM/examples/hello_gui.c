#define _POSIX_C_SOURCE 200809L
#include "hoswm.h"
#include <stdio.h>
#include <time.h>

#define MENU_GREET 1
#define MENU_NOTIFY 2
#define MENU_QUIT 3

int main(void) {
    HosWindow window=hos_gui_window_create("Hello GUI",360,180,0xff80afff);
    if(!window) {perror("create window");return 1;}
    if(hos_gui_label(window,1,16,16,328,24,"What is your name?")<0 ||
       hos_gui_textbox_ex(window,2,16,48,328,28,"hacker",true)<0 ||
       hos_gui_button(window,3,224,96,120,28,"Say hello")<0) {
        perror("add control");hos_window_close(window);return 1;
    }
    /* Menus are drawn in the bar at the top of the screen while this window
     * has focus; choosing an item arrives as HOS_EVENT_MENU. */
    const HosMenuItem items[]={
        {MENU_GREET,0,"Say hello","Enter"},
        {MENU_NOTIFY,0,"Send a notification",NULL},
        {0,HOS_MENU_SEPARATOR,NULL,NULL},
        {MENU_QUIT,0,"Quit",NULL},
    };
    const HosMenu menus[]={{"Hello",items,4}};
    if(hos_window_set_menus(window,menus,1)<0)perror("set menus");
    for(;;) {
        HosEvent event;int result=hos_window_poll(window,&event);
        if(result<0) {perror("poll window");hos_window_close(window);return 1;}
        if(event.kind==HOS_EVENT_CLOSED)break;
        if(event.kind==HOS_EVENT_MENU&&event.control==MENU_QUIT) {
            if(hos_message_box_ask("Quit","Close this window?",HOS_BUTTONS_YES_NO,HOS_QUESTION)==HOS_ANSWER_YES) {
                hos_window_close(window);break;
            }
        }
        if(event.kind==HOS_EVENT_MENU&&event.control==MENU_NOTIFY)
            if(hos_toast("Hello from the GUI example",0xff72dbac,0)<0)perror("toast");
        if((event.kind==HOS_EVENT_CLICK&&event.control==3)||event.kind==HOS_EVENT_SUBMIT||
           (event.kind==HOS_EVENT_MENU&&event.control==MENU_GREET)) {
            char name[1025],message[1100];
            if(hos_gui_get_text(window,2,name,sizeof(name))<0) {perror("read name");continue;}
            snprintf(message,sizeof(message),"Hello, %s!",name);
            if(hos_message_box_ask(message,"Say hello again?",HOS_BUTTONS_OK_CANCEL,HOS_INFO)==HOS_ANSWER_CANCEL)
                hos_toast("Suit yourself",0xffe4c878,2000);
        }
        struct timespec delay={0,16000000};nanosleep(&delay,NULL);
    }
    return 0;
}
