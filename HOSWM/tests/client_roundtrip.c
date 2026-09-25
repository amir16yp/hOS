#include "hoswm.h"
#include <assert.h>
#include <string.h>
int main(void) {
    uint32_t version,screen_width,screen_height;
    assert(hos_session_info(&version,&screen_width,&screen_height)==0);
    assert(version==HOSWM_ABI_VERSION&&screen_width==800&&screen_height==600);
    HosWindow w=hos_gui_window_create("C ABI test",240,120,0xff72dbac);
    assert(w!=0);
    assert(hos_gui_label(w,1,8,8,224,24,"Name")==0);
    assert(hos_gui_textbox(w,2,8,40,224,24,"first")==0);
    assert(hos_gui_label_ex(w,4,8,8,224,24,"Selectable label",true)==0);
    assert(hos_gui_textbox_ex(w,5,8,40,224,24,"Editable selection",true)==0);
    assert(hos_gui_control_ex(w,6,HOS_BUTTON,8,72,100,24,"Invalid",true)==-1);
    assert(hos_gui_button(w,3,8,72,100,24,"Submit")==0);
    assert(hos_gui_set_text(w,2,"updated")==0);
    char text[32];assert(hos_gui_get_text(w,2,text,sizeof(text))==0);
    assert(strcmp(text,"updated")==0);
    assert(hos_gui_remove_control(w,2)==0);
    assert(hos_gui_get_text(w,2,text,sizeof(text))==-1);
    assert(hos_gui_remove_control(w,2)==-1);
    assert(hos_gui_textbox(w,2,8,40,224,24,"recreated")==0);
    uint32_t width,height,minimized;assert(hos_window_size(w,&width,&height,&minimized)==0);
    assert(width==240&&height==120&&minimized==0);
    uint32_t pixels[240*120];for(size_t i=0;i<240*120;i++)pixels[i]=0xff111111;
    assert(hos_window_present(w,240,120,pixels)==0);
    assert(hos_window_present(w,239,120,pixels)==-1);
    /* Menu bar: valid menus are accepted, malformed ones are rejected. */
    const HosMenuItem file_items[]={
        {1,0,"Open","Ctrl+O"},
        {0,HOS_MENU_SEPARATOR,NULL,NULL},
        {2,HOS_MENU_DISABLED|HOS_MENU_CHECKED,"Save",NULL},
    };
    const HosMenu menus[]={{"File",file_items,3},{"Edit",NULL,0}};
    assert(hos_window_set_menus(w,menus,2)==0);
    assert(hos_window_set_menus(w,menus,0)==0);
    const HosMenuItem bad_item={0,0,"No identifier",NULL};
    const HosMenu bad_id={"File",&bad_item,1};
    assert(hos_window_set_menus(w,&bad_id,1)==-1);
    const HosMenu no_title={"",file_items,3};
    assert(hos_window_set_menus(w,&no_title,1)==-1);
    assert(hos_window_set_menus(w,menus,9)==-1);
    assert(hos_window_set_menus(w,menus,2)==0);
    /* Notifications do not belong to a window and need text. */
    assert(hos_toast("C client notification",0xff72dbac,1500)==0);
    assert(hos_toast("",0xff72dbac,0)==-1);
    HosEvent event;assert(hos_window_poll(w,&event)==0);
    HosWindow message=hos_message_box("Test message","It works",0xff80afff);
    assert(message!=0);assert(hos_window_close(message)==0);
    /* Answer dialogs validate their button set and severity. */
    HosWindow dialog=hos_message_box_open("Delete","Delete the file?",HOS_BUTTONS_YES_NO,HOS_QUESTION);
    assert(dialog!=0);
    assert(hos_message_box_open("Bad","buttons",9,HOS_INFO)==0);
    assert(hos_message_box_open("Bad","severity",HOS_BUTTONS_OK,9)==0);
    assert(hos_window_close(dialog)==0);
    assert(hos_window_close(w)==0);assert(hos_window_poll(w,&event)==1);
    assert(event.kind==HOS_EVENT_CLOSED);
    return 0;
}
