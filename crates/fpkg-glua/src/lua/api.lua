-- fpkg's GLua runtime: the names of the Garry's Mod server API (server and shared realms),
-- by library, with `_G` for the global functions, from nl's decl/glua.d.nl (the wiki data of
-- 2026-09-28). missing.lua makes every one of them that the runtime leaves out fail loudly.

return {
  ['ai'] = 'GetNodeCount GetScheduleID GetSquadLeader GetSquadMemberCount GetSquadMembers GetTaskID',
  ['ai_schedule'] = 'New',
  ['ai_task'] = 'New',
  ['baseclass'] = 'Get Set',
  ['cleanup'] = 'Add CC_AdminCleanup CC_Cleanup GetList GetTable Register ReplaceEntity',
  ['concommand'] = 'Add AutoComplete GetTable Remove Run',
  ['constraint'] = [[
    AddConstraintTable AddConstraintTableNoDelete AdvBallsocket Axis Ballsocket CanConstrain
    CreateKeyframeRope CreateStaticAnchorPoint Elastic Find FindConstraint FindConstraintEntity
    FindConstraints ForgetConstraints GetAllConstrainedEntities GetTable HasConstraints
    Hydraulic Keepupright Motor Muscle NoCollide Pulley RemoveAll RemoveConstraints Rope Slider
    Weld Winch
  ]],
  ['construct'] = 'Magnet SetPhysProp',
  ['cookie'] = 'Delete GetNumber GetString Set',
  ['cvars'] = 'AddChangeCallback Bool GetConVarCallbacks Number OnConVarChanged RemoveChangeCallback String',
  ['debug'] = 'Trace',
  ['debugoverlay'] = [[
    Axis Box BoxAngles Cross EntityTextAtPosition Grid Line ScreenText Sphere SweptBox Text
    Triangle
  ]],
  ['drive'] = [[
    CalcView CreateMove DestroyMethod End FinishMove GetMethod Move PlayerStartDriving
    PlayerStopDriving Register Start StartMove
  ]],
  ['duplicator'] = [[
    Allow ApplyBoneModifiers ApplyEntityModifiers ClearEntityModifier Copy CopyEntTable CopyEnts
    CreateConstraintFromTable CreateEntityFromTable Disallow DoBoneManipulator DoFlex DoGeneric
    DoGenericPhysics FigureOutRequiredAddons FindEntityClass GenericDuplicatorFunction
    GetAllConstrainedEntitiesAndConstraints IsAllowed Paste RegisterBoneModifier
    RegisterConstraint RegisterEntityClass RegisterEntityModifier RemoveMapCreatedEntities
    SetLocalAng SetLocalPos StoreBoneModifier StoreEntityModifier WorkoutSize
  ]],
  ['effects'] = 'BeamRingPoint BubbleTrail Bubbles',
  ['engine'] = [[
    AbsoluteFrameTime ActiveGamemode CloseServer GetAddons GetGamemodes GetGames LightStyle
    TickCount TickInterval
  ]],
  ['ents'] = [[
    Create FindAlongRay FindByClass FindByClassAndParent FindByModel FindByName FindInBox
    FindInCone FindInPVS FindInSphere FireTargets GetAll GetByIndex GetCount GetEdictCount
    GetMapCreatedEntity Iterator
  ]],
  ['file'] = 'Append AsyncRead CreateDir Delete Exists Find IsDir Open Read Rename Size Time Write',
  ['game'] = [[
    AddAmmoType AddDecal AddParticles BuildAmmoTypes CleanUpMap ConsoleCommand Get3DSkyboxInfo
    GetAmmoDamageType GetAmmoData GetAmmoForce GetAmmoID GetAmmoMax GetAmmoNPCDamage GetAmmoName
    GetAmmoPlayerDamage GetAmmoTypes GetGlobalCounter GetGlobalState GetIPAddress GetMap
    GetMapChangeCount GetMapNext GetMapVersion GetSkillLevel GetTimeScale GetWindSpeed GetWorld
    IsDedicated KickID LoadNextMap MapLoadType MaxPlayers MountGMA RemoveRagdolls
    SetGlobalCounter SetGlobalState SetSkillLevel SetTimeScale SinglePlayer StartSpot
  ]],
  ['gameevent'] = 'Listen',
  ['gamemode'] = 'Call Get Register',
  ['gmod'] = 'GetGamemode',
  ['gmsave'] = 'LoadMap PlayerLoad PlayerSave SaveMap ShouldSaveEntity',
  ['hammer'] = 'SendCommand',
  ['hook'] = 'Add Call GetTable Remove Run',
  ['http'] = 'Fetch Post',
  ['list'] = 'Add Contains Get GetEntry GetForEdit GetTable HasEntry RemoveEntry Set',
  ['math'] = [[
    AngleDifference Approach ApproachAngle BSplinePoint BinToInt CHSpline Clamp CubicBezier Dist
    Distance DistanceSqr EaseInOut Factorial IntToBin IsNearlyEqual NormalizeAngle
    QuadraticBezier Rand Remap Round Sign SnapTo TimeFraction Truncate calcBSplineN
  ]],
  ['math.ease'] = [[
    InBack InBounce InCirc InCubic InElastic InExpo InOutBack InOutBounce InOutCirc InOutCubic
    InOutElastic InOutExpo InOutQuad InOutQuart InOutQuint InOutSine InQuad InQuart InQuint
    InSine OutBack OutBounce OutCirc OutCubic OutElastic OutExpo OutQuad OutQuart OutQuint
    OutSine
  ]],
  ['motionsensor'] = 'BuildSkeleton ChooseBuilderFromEntity ProcessAngle ProcessAnglesTable ProcessPositionTable',
  ['navmesh'] = [[
    AddWalkableSeed BeginGeneration ClearWalkableSeeds CreateNavArea CreateNavLadder Find
    FindInBox GetAllNavAreas GetBlockedAreas GetEditCursorPosition GetGroundHeight GetMarkedArea
    GetMarkedLadder GetNavArea GetNavAreaByID GetNavAreaCount GetNavLadderByID GetNearestNavArea
    GetPlayerSpawnName IsGenerating IsLoaded Load Reset Save SetMarkedArea SetMarkedLadder
    SetPlayerSpawnName
  ]],
  ['net'] = [[
    Abort Broadcast BytesLeft BytesWritten Incoming ReadAngle ReadBit ReadBool ReadColor
    ReadData ReadDouble ReadEntity ReadFloat ReadHeader ReadInt ReadMatrix ReadNormal ReadPlayer
    ReadString ReadTable ReadType ReadUInt ReadUInt64 ReadVector Receive Send SendOmit SendPAS
    SendPVS Start WriteAngle WriteBit WriteBool WriteColor WriteData WriteDouble WriteEntity
    WriteFloat WriteInt WriteMatrix WriteNormal WritePlayer WriteString WriteTable WriteType
    WriteUInt WriteUInt64 WriteVector
  ]],
  ['numpad'] = 'Activate Deactivate FromButton OnDown OnUp Register Remove Toggle',
  ['physenv'] = [[
    AddSurfaceData GetAirDensity GetGravity GetLastSimulationTime GetPerformanceSettings
    GetPhysicsPaused GetTimeScale SetAirDensity SetGravity SetPerformanceSettings
    SetPhysicsPaused SetTimeScale
  ]],
  ['player'] = [[
    CreateNextBot GetAll GetBots GetByAccountID GetByID GetBySteamID GetBySteamID64
    GetByUniqueID GetCount GetCountConnecting GetHumans Iterator
  ]],
  ['player_manager'] = [[
    AddValidHands AddValidModel AllValidModels ClearPlayerClass GetAllPlayerModels
    GetPlayerClass GetPlayerClassTable GetPlayerClasses OnPlayerSpawn RegisterClass
    RemoveValidModel RunClass SetPlayerClass TranslatePlayerHands TranslatePlayerModel
    TranslateToPlayerModelName
  ]],
  ['properties'] = 'Add CanBeTargeted OnScreenClick OpenEntityMenu Remove',
  ['resource'] = 'AddFile AddSingleFile AddWorkshop',
  ['saverestore'] = [[
    AddRestoreHook AddSaveHook LoadEntity LoadGlobal PreRestore PreSave ReadTable ReadVar
    SaveEntity SaveGlobal WritableKeysInTable WriteTable WriteVar
  ]],
  ['scripted_ents'] = 'Alias Get GetList GetMember GetSpawnable GetStored GetType IsBasedOn OnLoaded Register',
  ['sound'] = [[
    Add AddSoundOverrides EmitHint GetLoudestSoundHint GetProperties GetTable Play
    SetActorGender
  ]],
  ['sql'] = 'Begin Commit IndexExists LastError Query QueryRow QueryTyped QueryValue SQLStr TableExists',
  ['steamworks'] = 'FileInfo GetList',
  ['string'] = [[
    CardinalToOrdinal Comma EndsWith Explode FormattedTime FromColor GetChar
    GetExtensionFromFilename GetFileFromFilename GetPathFromFilename Implode Interpolate
    JavascriptSafe Left NiceName NiceSize NiceTime PatternSafe Replace Right SetChar Split
    StartWith StartsWith StripExtension ToColor ToMinutesSeconds ToMinutesSecondsMilliseconds
    ToTable Trim TrimLeft TrimRight
  ]],
  ['system'] = 'AppTime BatteryPower GetCountry HasFocus IsLinux IsOSX IsWindows SteamTime UpTime',
  ['table'] = [[
    Add ClearKeys CollapseKeyValue Copy CopyFromTo Count DeSanitise Empty FindNext FindPrev Flip
    ForEach ForceInsert GetFirstKey GetFirstValue GetKeys GetLastKey GetLastValue GetWinningKey
    HasValue Inherit IsEmpty IsSequential KeyFromValue KeysFromValue LowerKeyNames
    MemberValuesFromKey Merge Pack Random RemoveByValue Reverse Sanitise Shuffle SortByKey
    SortByMember SortDesc ToString
  ]],
  ['team'] = [[
    AddScore BestAutoJoinTeam GetAllTeams GetClass GetColor GetName GetPlayers GetScore
    GetSpawnPoint GetSpawnPoints Joinable NumPlayers SetClass SetColor SetScore SetSpawnPoint
    SetUp TotalDeaths TotalFrags Valid
  ]],
  ['timer'] = [[
    Adjust Check Create Destroy Exists IsPaused Pause Remove RepsLeft Simple Start Stop TimeLeft
    Toggle UnPause
  ]],
  ['umsg'] = 'Angle Bool Char End Entity Float Long PoolString Short Start String Vector VectorNormal',
  ['undo'] = [[
    AddEntity AddFunction Create Do_Undo Finish GetTable ReplaceEntity SetCustomUndoText
    SetPlayer
  ]],
  ['usermessage'] = 'GetTable Hook IncomingMessage',
  ['utf8'] = 'GetChar',
  ['util'] = [[
    AddNetworkString AimVector Base64Decode Base64Encode BlastDamage BlastDamageInfo CRC
    Compress DateStamp Decal DecalMaterial Decompress DistanceToLine Effect GetActivityIDByName
    GetActivityNameByID GetAnimEventIDByName GetAnimEventNameByID GetModelInfo GetModelMeshes
    GetPData GetPlayerTrace GetSurfaceData GetSurfaceIndex GetSurfacePropName GetUserGroups
    IntersectRayWithOBB IntersectRayWithPlane IntersectRayWithSphere IntersectRayWithTriangle
    IsBinaryModuleInstalled IsBoxIntersectingBox IsBoxIntersectingSphere IsInWorld IsModelLoaded
    IsOBBIntersectingOBB IsPointInCone IsRayIntersectingRay IsSphereIntersectingCone
    IsSphereIntersectingSphere IsValidModel IsValidPhysicsObject IsValidProp IsValidRagdoll
    JSONToTable KeyValuesToTable KeyValuesToTablePreserveOrder LocalToWorld MD5
    NetworkIDToString NetworkStringToID NiceFloat ParticleTracer ParticleTracerEx PointContents
    PrecacheModel PrecacheSound QuickTrace RemovePData SHA1 SHA256 ScreenShake SetPData
    SharedRandom SpriteTrail Stack SteamIDFrom64 SteamIDTo64 StringToType TableToJSON
    TableToKeyValues Timer TimerCycle TraceEntity TraceEntityHull TraceHull TraceLine
    TypeToString
  ]],
  ['weapons'] = 'Get GetList GetStored IsBasedOn OnLoaded Register',
  ['widgets'] = 'PlayerTick',
  ['_G'] = [[
    AccessorFunc AddCSLuaFile AddConsoleCommand AddOriginToPVS Add_NPC_Class Angle AngleRand
    BroadcastLua BuildNetworkedVarsTable Color ColorAlpha ColorRand ColorToHSL ColorToHSV
    CompileFile CompileString ConVarExists CreateClientConVar CreateConVar CreatePhysCollideBox
    CreatePhysCollidesFromModel CreateSound CurTime DEFINE_BASECLASS DTVar_ReceiveProxyGL
    DamageInfo DebugInfo DeriveGamemode DropEntityIfHeld EffectData Either EmitSentence
    EmitSound Entity ErrorNoHalt ErrorNoHaltWithStack FindMetaTable Format FrameNumber FrameTime
    GetConVar GetConVarNumber GetConVarString GetConVar_Internal GetGlobal2Angle GetGlobal2Bool
    GetGlobal2Entity GetGlobal2Float GetGlobal2Int GetGlobal2String GetGlobal2Var
    GetGlobal2Vector GetGlobalAngle GetGlobalBool GetGlobalEntity GetGlobalFloat GetGlobalInt
    GetGlobalString GetGlobalVar GetGlobalVector GetHostName GetPredictionPlayer HSLToColor
    HSVToColor HTTP HWBToColor HexToColor IncludeCS InvalidateInternalEntityCache IsColor
    IsConCommandBlocked IsEnemyEntityName IsEntity IsFirstTimePredicted IsFriendEntityName
    IsMounted IsTableOfEntitiesValid IsUselessModel IsValid Lerp LerpAngle LerpVector
    LocalToWorld MakeBalloon Material Matrix Model Msg MsgAll MsgC MsgN OrderVectors Particle
    ParticleEffect ParticleEffectAttach Path Player PrecacheParticleSystem PrecacheScene
    PrecacheSentenceFile PrecacheSentenceGroup PrintMessage PrintTable ProtectedCall RandomPairs
    RealTime RecipientFilter RegisterMetaTable RunConsoleCommand RunString RunStringEx SQLStr
    STNDRD SafeRemoveEntity SafeRemoveEntityDelayed SendUserMessage SentenceDuration ServerLog
    SetGlobal2Angle SetGlobal2Bool SetGlobal2Entity SetGlobal2Float SetGlobal2Int
    SetGlobal2String SetGlobal2Var SetGlobal2Vector SetGlobalAngle SetGlobalBool SetGlobalEntity
    SetGlobalFloat SetGlobalInt SetGlobalString SetGlobalVar SetGlobalVector
    SetPhysConstraintSystem SortedPairs SortedPairsByMemberValue SortedPairsByValue Sound
    SoundDuration SuppressHostEvents SysTime TauntCamera TimedCos TimedSin TypeID
    UTIL_IsUselessModel UnPredictedCurTime VGUIFrameTime Vector VectorRand WorldToLocal module
  ]],
}
