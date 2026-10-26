use super::{ei, Shared};
use ashpd::desktop::input_capture::{
    Barrier, BarrierID, BarrierPosition, Capabilities, CreateSessionOptions, InputCapture,
};
use futures_util::StreamExt;
use std::{sync::Arc, time::Duration};

pub(super) async fn run(shared: Arc<Shared>, generation: u64) -> Result<(), String> {
    let portal = InputCapture::new()
        .await
        .map_err(|e| format!("InputCapture: {e}"))?;
    let setup = portal.create_session(
        None,
        CreateSessionOptions::default()
            .set_capabilities(Capabilities::Keyboard | Capabilities::Pointer),
    );
    tokio::pin!(setup);
    // Portal requests may wait for user interaction. Cancellation drops the request,
    // and each subsequent stage checks the generation before capturing anything.
    let (session, capabilities) = loop {
        tokio::select! {
            response=&mut setup=>break response.map_err(|e|format!("Autorización de captura: {e}"))?,
            _=tokio::time::sleep(Duration::from_millis(20))=>{
                if !shared.running(generation){return Ok(());}
            }
        }
    };
    let result=async{
        if !capabilities.contains(Capabilities::Pointer)||!capabilities.contains(Capabilities::Keyboard){
            return Err("El compositor debe permitir capturar teclado y mouse para ofrecer liberación segura".into());
        }
        if !shared.running(generation){return Ok(());}
        let fd=portal.connect_to_eis(&session,Default::default()).await.map_err(|e|e.to_string())?;
        let mut receiver=ei::Receiver::new(fd)?;
        let readiness_fd=receiver.readiness_fd()?;
        // SAFETY: AsyncFd owns this duplicated OwnedFd. Its raw descriptor stays
        // open and unchanged until AsyncFd drops, regardless of libei's original.
        let readiness=unsafe{tokio::io::unix::AsyncFd::register(readiness_fd)}.map_err(|e|e.to_string())?;
        let zones=portal.zones(&session,Default::default()).await.map_err(|e|e.to_string())?
            .response().map_err(|e|e.to_string())?;
        // Right boundaries outside the combined desktop are valid. The compositor
        // rejects internal edges; any accepted external edge is sufficient.
        let barriers:Vec<_>=zones.regions().iter().enumerate().map(|(index,region)|{
            let x=region.x_offset()+region.width() as i32;
            let y=region.y_offset();
            Barrier::new(BarrierID::new(index as u32+1).expect("nonzero barrier"),
                BarrierPosition::new(x,y,x,y+region.height() as i32-1))
        }).collect();
        if barriers.is_empty(){return Err("El compositor no informó pantallas para capturar".into());}
        let request=portal.set_pointer_barriers(&session,&barriers,zones.zone_set(),Default::default())
            .await.map_err(|e|e.to_string())?;
        let response=request.response().map_err(|e|e.to_string())?;
        if response.failed_barriers().len()==barriers.len(){
            return Err("El compositor rechazó los bordes de captura".into());
        }
        let mut activated=portal.receive_activated().await.map_err(|e|e.to_string())?;
        let mut deactivated=portal.receive_deactivated().await.map_err(|e|e.to_string())?;
        let mut disabled=portal.receive_disabled().await.map_err(|e|e.to_string())?;
        let mut zones_changed=portal.receive_zones_changed().await.map_err(|e|e.to_string())?;
        if !shared.running(generation){return Ok(());}
        portal.enable(&session,Default::default()).await.map_err(|e|e.to_string())?;
        shared.report(false,"Wayland · libei","Captura preparada: mueve el cursor al borde derecho de la pantalla. Ctrl+Alt+Esc libera el control.");
        let mut active=false;
        let mut ticker=tokio::time::interval(Duration::from_millis(20));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop{
            tokio::select!{
                signal=activated.next()=>{
                    if signal.is_none(){return Err("El portal dejó de enviar activaciones".into());}
                    if !shared.running(generation){break;}
                    active=true;
                    shared.report(true,"Wayland · libei","Control remoto activo · Ctrl+Alt+Esc para liberar");
                }
                signal=deactivated.next()=>{
                    if signal.is_none(){return Err("El portal de captura se desconectó".into());}
                    active=false;shared.release();
                    shared.report(false,"Wayland · libei","Captura liberada por el compositor; mueve el cursor al borde derecho para activar");
                }
                _=disabled.next()=>{break;}
                _=zones_changed.next()=>{
                    return Err("Cambió la disposición de las pantallas; vuelve a activar la captura".into());
                }
                _=ticker.tick()=>{
                    if !shared.running(generation){break;}
                }
                ready=readiness.readable()=>{
                    let mut guard=ready.map_err(|e|e.to_string())?;
                    if !shared.running(generation){break;}
                    for event in receiver.events()?{
                        // Drain setup events even before activation (bind seats).
                        if !active{continue;}
                        match event{
                            ei::Event::Motion(dx,dy)=>shared.motion(dx,dy)?,
                            ei::Event::Button(bit,pressed)=>shared.button(bit,pressed)?,
                            ei::Event::Wheel(steps)=>shared.wheel(steps)?,
                            ei::Event::Key(code,pressed)=>{
                                if shared.evdev_key(code,pressed)?{return Ok(());}
                            }
                            ei::Event::Stopped=>{shared.release();}
                        }
                    }
                    guard.clear_ready();
                }
            }
        }
        Ok(())
    }.await;
    // Closing a session always releases the compositor's grab, even if negotiation,
    // hotplug, transport, or queue processing failed. No privileged evdev access.
    let _ = portal.disable(&session, Default::default()).await;
    let _ = session.close().await;
    result
}
